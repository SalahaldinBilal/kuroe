# kuroe

A self-hosted deployment server for Docker hosts. One Rust binary (or one container) deploys images, Git repos (Dockerfile or Compose), Dockerfiles written in the UI, uploaded build contexts, Compose files and static sites (from a repo, an image or an uploaded archive), fronts them with a managed Traefik or config for your own web server, enforces resource pools, keeps logs and metrics, and gives a browser shell, file browser and port preview into containers. The SolidJS SPA is embedded in the binary.

## Layout

```
src/
  main.rs        clap entry: serve, migrate, admin (setup-code, reset-password, http open|local, rotate-key), self-update
  startup.rs     serve's startup (applies migrations), and the fallback that shows a failed start in the UI
  api/           axum routers and handlers, one file per resource; mod.rs has every route and AppState
    access.rs    per-deployment and per-container permission checks
    events.rs    /events/ws, the live events socket
    http_access.rs  plain HTTP open or local only
    mcp.rs       read-only MCP server at /api/v1/mcp
    ci.rs, pipelines.rs  push webhooks and generated CI configs
    preview.rs   /preview/<container>/<port>/ reverse proxy
    tests.rs     API tests (need the Docker socket, see Commands)
  auth/          sessions, roles and permissions (rbac.rs), API tokens, passwords, login rate limit
  deploy/        spec (sources, site options, secret_refs), runner (deploys, pool, limits), deploy tokens, grants
  docker/        bollard wrappers, compose override writer (rewrite.rs), admission check (pool), network limits (netlimit.rs)
  proxy/         Traefik container and route files, certificates, blue/green, web server configs (webserver/)
  collect/       container stats, host metrics, logs, storage accounting, retention
  identity/      saved logins: git hosts, registries, Cloudflare, certificates
  runners/       self-hosted CI runner pools
  update/        self-update (container.rs, binary.rs) and the GitHub release check (release.rs)
  events.rs      live events: the broadcast channel and the Docker events watcher
  git.rs         clones through gix
  sites.rs       static sites: the shared nginx container (kuroe_static), builds, revisions, routes
  saved_secrets.rs  named secrets deployments point at (the only code touching that table)
  volumes.rs     volume sizes and soft limits, and the check that stops deployments over a limit
  secrets.rs     sealing at rest, fingerprints, key rotation
  bundle.rs      TOML export/import, including what an export leaves out
  db/            sqlx pool, migrations check, now(), helpers
  web.rs         serves the embedded SPA from web/dist
tests/           integration tests against real Docker and Traefik, #[ignore]d by default
migrations/      numbered sqlx migrations (up and down)
web/             SolidJS SPA (Vite, TypeScript, SCSS, Biome, Vitest)
scripts/         bump-version.ts, run through the root package.json
.github/workflows/  ci.yml (push and PR) and release.yml (v* tags)
Dockerfile       node build, rust build, debian:stable-slim runtime with docker-cli + compose plugin
install.sh       one-line installer (container, or --systemd for the binary)
kuroe.service    systemd unit for the binary install, shipped with each release
CHANGELOG.md     release notes, the release workflow reads the version's section
```

## Commands

Frontend, from `web/`:
- `npm run build`: type check (`tsc --noEmit`) and Vite build into `web/dist`.
- `npm run lint`: Biome on `src`. `npx biome check --write src` applies safe fixes and formatting.
- `npm test`: Vitest (jsdom). Solid hot reload is off under Vitest in `vite.config.ts`, otherwise component tests can't load.

Backend: `web/dist` must exist before `cargo build`, because `rust-embed` compiles it into the binary. Run tests in the Linux Rust image with the Docker socket mounted, from the repo root. The API tests create a real `AppState`, which connects to Docker:

```
docker run --rm -v /var/run/docker.sock:/var/run/docker.sock \
  -v "$PWD":/src \
  -v kuroe_cargo_registry:/usr/local/cargo/registry -v kuroe_cargo_target:/src/target \
  -w /src rust:1.99-slim cargo test --locked
```

- One test: `cargo test --locked --lib <name>`.
- The Docker-heavy tests (Traefik routing and certificates, exec over websocket, the file browser, a few in `src/`) are `#[ignore]`d. Run them with `cargo test --locked -- --ignored`. `tests/metrics_volume.rs` writes millions of rows, run it with `--release`.
- Lint: `cargo clippy --all-targets --locked -- -D warnings`. CI fails on any warning. The `rust:1.99-slim` image needs `rustup component add clippy` first.
- `rustfmt` is not used yet. The code doesn't follow one line width, so don't run `cargo fmt` over it.

CI (`.github/workflows/ci.yml`) runs on pushes to `main` and on PRs: web lint, tests and build, then clippy and `cargo test` on the built `web/dist`.

Releases: add notes under `## [Unreleased]` in `CHANGELOG.md`, run `npm run version:bump <version>` from the repo root (sets `Cargo.toml`, `Cargo.lock` and the changelog section), commit, and push the tag `v<version>`. `.github/workflows/release.yml` checks the tag against `Cargo.toml`, runs CI, then publishes `kuroe-linux-amd64` and `kuroe-linux-arm64` with their `.sha256` files and `kuroe.service` to a GitHub release, and the multi-arch image to `ghcr.io/salahaldinbilal/kuroe` as `<version>` and `latest`.

A local instance from the repo root:

```
docker build -t kuroe:dev .
docker run -d --name kuroe_dev -p 27863:27863 -v kuroe_dev_data:/data -v /var/run/docker.sock:/var/run/docker.sock kuroe:dev serve
```

## Backend notes

- SQLite through sqlx. Schema changes only go in a new migration under `migrations/`. Never create or alter tables from application code.
- `0001_init` is the whole schema as of 0.1.0. Never edit a migration once it's released: sqlx checks each applied migration's checksum and refuses a changed one. New changes get the next number, with a down migration. `serve` applies pending migrations on start, and refuses a database migrated by a newer kuroe (`db::migrate`). The tests in `db/mod.rs` list the tables each migration adds (`TABLES_ADDED`).
- Every handler checks access: `current.require(Permission::...)` for server-wide actions, `access::deployment` / `access::container` with a `dep.*` permission for anything tied to a deployment. Roles live in the `roles` table, `auth::Access` (loaded per request into `CurrentUser`) resolves a grant's role first, then the user's role and its scope. The owner always has everything. Never compare role names to decide access, except `is_owner()`.
- Role scopes: `all`, a list of deployment ids, or `granted`. `granted` still gives `dep.view` on every deployment, and grants add the rest. Only a list scope hides deployments outside it.
- Live events (`src/events.rs`): code that changes a deployment's status, spec or domains sends an `Event` through `events::send`, or `events::changed(id)` for a plain change. `deploy::create`, `update_spec`, `begin_deploy`, `finish`, `delete` and the domain functions in `proxy/mod.rs` already do. Container start and die (deployments, runners, Traefik) come from the Docker events watcher. Each socket passes on only what `Event::visible_to` allows and reloads the user's access every minute. A new kind goes into the `Event` enum, `visible_to` and `LiveEvent` in `web/src/states/eventState.ts`. Events carry ids and states, never secrets or log lines.
- Version: `Cargo.toml` is the only source (`update::VERSION`). `KUROE_BUILD` at compile time adds a build label (the short commit in releases).
- Release files are named `kuroe-linux-<amd64|arm64>`, each with a `<name>.sha256` and `kuroe.service` next to it, and the image is `ghcr.io/salahaldinbilal/kuroe:<version>`. `release.yml`, `install.sh`, binary self-update and `update::release` (the release check behind `GET /update/latest`) all depend on these names. Change them together.
- The default port is 27863 (`serve`, the Dockerfile, `kuroe.service`, `install.sh`, the Vite dev proxy).
- State-changing requests need the `x-kuroe-csrf` header, sessions are an opaque cookie.
- First-run setup needs the one-time code in `<data>/setup-code` (`auth::setup_code`). `serve` creates it while no user exists and logs it on every start with the searchable text `Setup code:`. `kuroe admin setup-code` prints it, and `install.sh` shows it after installing. The setup handler deletes it once the owner exists, and wrong codes count against the login rate limit.
- Everything kuroe creates in Docker carries the `kuroe.managed=true` label. Shared volumes are `kuroe_<name>`, the proxy network is `kuroe_proxy`, Traefik is the `kuroe_traefik` container.
- Resource limits are hard caps passed to Docker. The pool check (`Runner::running_usage`, `admission::admit`) counts each deployment or runner container's peak use over the last 5 minutes from the 10 s samples (`container_metrics`, `cpu_percent` where 100 is one core), or its limits while it has no samples. kuroe's own containers (Traefik, `kuroe_static`, builds) are not in the pool.
- Saved secrets: a spec points at one through `spec.secret_refs` (env key, or `@git_token` for the git token, to the saved secret id). The referenced env value stays empty (`clear_referenced_values`) and is filled only at deploy time (`saved_secrets::resolve`). Never copy a saved value into a spec. Using one needs `secret.use`, managing one `secret.manage`.
- A domain's `options` (`proxy::options::RouteOptions`) hold paths to other services, a redirect, compression, error pages, retries, a timeout, a body limit and a Host rewrite. Only managed Traefik applies them. Traefik service names come from `routes::service_key`. A redirect domain has an empty service and port 0. Error pages are files in `kuroe_static` under `/_errors/<deployment id>/`, rewritten on every `publish_routes`.
- Compose deployments get `spec.container.env` (with saved secrets resolved) as the environment of `docker compose config`, so `${VAR}` in the Compose file fills from it.
- Static sites are `spec.site` on a git, image or `static` (uploaded archive) source. They run in the shared `kuroe_static` nginx container, not as their own deployment containers.
- Exports never carry webhook secrets, deploy token values, login secrets or saved secret values, and only carry env secrets with "include secrets". Whatever is left out is named in the bundle so the import asks for it or makes a new one (`bundle::left_out`).
- Axum 0.8: a `{*rest}` catch-all doesn't match an empty path. A route that must also answer at a trailing slash needs its own exact route (see `/preview/{container}/{port}/`).
- Identity secrets, secret env values, git tokens, webhook secrets and saved secrets are sealed at rest (`src/secrets.rs`). Read and write them through `deploy::get`/`create`/`update_spec`, `deploy::new_webhook_secret`, `identity::get`/`insert`, the `saved_secrets` module or `secrets::sealer()`, never with raw SQL. A new kind of sealed value also goes into `secrets::stored`, `seal_plain` and `reseal`. The key is `KUROE_SECRET_KEY_FILE` or `<data>/secret.key`.
- Every API route lives under `/api/v1` (`API_BASE` in `src/api/mod.rs` and `web/src/api.ts`).
- Anything that doesn't match a route falls through to the SPA, which serves `index.html`. A missing backend route therefore shows up as a blank page, not a 404.

## Frontend notes

- Solid 1.9 and `@solidjs/router` with `explicitLinks`: the router only handles its own `<A>` links. In-app navigation uses `<A>` or `Button` with an `href` (which renders `<A>` for app paths). Links to `/api/...` or `/preview/...` stay plain links, and downloads use `Button download`.
- UI controls are ported components, each in `web/src/components/ui/<Name>/<Name>.tsx` with a `<Name>.module.scss`: Button, Input, Select, Switch, SegmentedControl, Field, Modal, EmptyState, Spinner, Toasts, Tooltip, ContextMenu, Tabs, ProgressBar, Drawer, SettingRow. Use them. Don't add native `<select>`, bare styled `<input>` or hand-rolled buttons. Slider and Tiles in `ui/Form.tsx` are kuroe's own, because no ported component covers them.
- Shared state is in `web/src/states/`: toasts (`useToastState`, `pushToast` / `pushError`), tooltips, context menu, the saved secrets list (`savedSecretState`), live events (`eventState`). Hosts for the first three are mounted once in `App.tsx`, which also starts and stops the events socket with the session.
- Any field that can hold a secret uses `SecretValue` (`components/SecretValue.tsx`): a plain value, a raw secret with a "raw" tag, or a saved secret reference shown as a chip. Pasting a value that matches a saved secret links to it.
- Settings pages (Settings, Proxy, service settings) use cards of `SettingRow`s with selects, segmented controls and inputs. Big `Tiles` are for first-time choices like New deployment and onboarding, not for settings pages.
- Pages that list things by kind (Identities) only show a section once it has something in it, with one empty state for the whole page.
- A save that can take a while (starting Traefik, deploys) shows what it's doing: `SaveBar` takes `busyMessage` and shows a spinner and the time so far.
- Tooltips: `Button tooltip="..."`, or wrap anything else in `Tip`. Don't use `title` attributes.
- Icons: the inline `Icon` component in `ui/Icon.tsx`. No icon package.
- The kuroe mark (a stencil K with three dorsal plates) is `Logo` in `ui/Logo.tsx`, used in every `.brand`, and `web/public/favicon.svg`. Both draw the same paths, so change them together.
- Styles: tokens in `web/src/styles/tokens.scss`, including aliases with the names the ported components expect. Page layout classes in `app.scss`, shared display classes (cards, tables, pills, notes, charts, row editors) in `components.scss`. Light and dark both follow the system setting.
- Read resources with `ready(resource)` from `ui/Overlay.tsx`. It returns `undefined` instead of throwing when the request failed.
- Forms with a save bar compare the draft against the saved values with `same()`. They don't use a dirty flag, so reverting a change hides the bar.
- `web/src/spec.ts` converts between the API spec shapes and the row editors (ports, env with secret flags, mounts).
- Charts (`LineChart`) take the full range as `domain` and a `gap` (from `gapFor(bucket, sampleSeconds)`) so missing data shows as a gap, not a line.
- Changes that happen at odd times (deploys, containers starting or stopping, domains, runners) come over the events socket. Pages call `onEvent(match, refetch, EVENT_DEBOUNCE_MS)` from `states/eventState.ts`. After a reconnect every handler gets a `resync` and should refetch. While `eventsConnected()` is false, pages fall back to timed reloads, like `useLive(() => !eventsConnected() && refetch())`.
- Data sampled on a clock (metrics, pool usage, storage) reloads with `useLive` (`web/src/live.ts`): an interval matched to the sample rate, paused in hidden tabs. Use its quiet flag so background reloads don't show a loading state.

## Style

- No code comments unless the code can't explain itself (a non-obvious constraint or a workaround). One short line when needed.
- In TypeScript, use `function` declarations, except for short one-liners.
- No em dashes or semicolons as punctuation in prose, comments or UI copy. Write UI copy and docs in plain, short sentences, with no marketing tone.
- UI: controls in a row line up on their vertical centers and share a height. Empty states and placeholders sit centered.
- Git: read-only commands only (`status`, `diff`, `log`, `show`, `blame`). The user commits.
