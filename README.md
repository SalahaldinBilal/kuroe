# kuroe

A self-hosted deployment server for a Docker host. It is one Rust binary, or one container, with the web UI built in.

## What it does

- Deploys images from a registry, Git repositories (with a Dockerfile or a Compose file), Dockerfiles written in the UI, uploaded build contexts and Compose files.
- Serves static sites from a repository, an image or an uploaded archive.
- Puts deployments behind a managed Traefik with domains, certificates (Let's Encrypt, saved certificates, Cloudflare origin certificates), redirects, paths, compression and error pages. Or it writes config for a web server you already run (nginx, Apache, Caddy, HAProxy, lighttpd, Traefik).
- Zero-downtime blue/green deploys.
- A resource pool with CPU, memory and network limits per deployment.
- Logs, container and host metrics, and storage use, with retention.
- A browser shell, a file browser and a port preview for every container.
- Deploys from CI: deploy tokens, push webhooks (GitHub, GitLab, Gitea, Forgejo), generated pipelines and self-hosted CI runners.
- Users, custom roles and per-deployment grants.
- Saved logins for Git hosts, registries and Cloudflare, and named secrets that deployments point at. Secrets are encrypted at rest.
- Export and import of deployments as TOML.
- A read-only MCP server at `/api/v1/mcp`.

## Install

You need Docker on the host.

As a container (the default):

```sh
curl -fsSL https://raw.githubusercontent.com/SalahaldinBilal/kuroe/main/install.sh | sh
```

As a systemd service running the binary (Linux, as root, amd64 or arm64):

```sh
curl -fsSL https://raw.githubusercontent.com/SalahaldinBilal/kuroe/main/install.sh | sh -s -- --systemd
```

Then open `http://<host>:27863` and create the owner account with the setup code the installer printed.

### Setup code

Creating the owner account needs a one-time setup code, so nobody else who reaches the port first can take the server. kuroe makes the code on its first start, prints it on every start until the owner exists, and deletes it once the owner is created. The code looks like `ABCD-EFGH-JKLM`.

To see it again:

| Install | Command |
|---|---|
| Container | `docker exec kuroe kuroe admin setup-code` |
| systemd | `sudo kuroe --data-dir /var/lib/kuroe admin setup-code` |
| Container log | `docker logs kuroe 2>&1 \| grep "Setup code"` |
| systemd log | `sudo journalctl -u kuroe \| grep "Setup code"` |

It's also in the file `setup-code` in the data folder (`/data` in the container, `/var/lib/kuroe` with systemd).

kuroe listens on all interfaces and doesn't change the firewall. Once the owner exists, the port shows only the sign-in page.

The installer reads these variables:

| Variable | Default | |
|---|---|---|
| `KUROE_IMAGE` | `ghcr.io/salahaldinbilal/kuroe:latest` | image for the container install |
| `KUROE_PORT` | `27863` | host port for the container install |
| `KUROE_BINARY_URL` | the latest release for this machine | binary for `--systemd`. A `.sha256` file and `kuroe.service` must sit next to it |

To run the container by hand:

```sh
docker run -d --name kuroe --restart unless-stopped \
  -p 27863:27863 \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v kuroe_data:/data \
  ghcr.io/salahaldinbilal/kuroe:latest
```

## Configuration

| Variable | Flag | Default | |
|---|---|---|---|
| `KUROE_DATA` | `--data-dir` | `data` (`/data` in the image, `/var/lib/kuroe` with systemd) | database, keys, certificates |
| `KUROE_LISTEN` | `serve --listen` | `0.0.0.0:27863` | address the UI and API listen on |
| `KUROE_SECURE_COOKIES` | `serve --secure-cookies` | off | mark session cookies Secure even on plain HTTP |
| `KUROE_NO_MIGRATE` | `serve --no-migrate` | off | refuse to start with pending migrations instead of applying them |
| `KUROE_SECRET_KEY_FILE` | | `<data>/secret.key` | key that encrypts secrets at rest |
| `RUST_LOG` | | `info` | log filter |

Back up the data folder, and the secret key if you keep it elsewhere. Without the key, saved secrets, logins and secret env values can't be read.

## Commands

```
kuroe serve                        run the server
kuroe migrate                      apply database migrations (serve does this on start)
kuroe admin setup-code             print the one-time code for creating the owner account
kuroe admin reset-password <user>  set a new random password and sign the user out
kuroe admin http open|local        open plain HTTP again, or limit it to this machine and HTTPS proxies
kuroe admin rotate-key             encrypt every secret again with a new key
```

In the container install, run them with `docker exec kuroe kuroe <command>`. With systemd, run them as root with the data folder: `sudo kuroe --data-dir /var/lib/kuroe <command>`.

## Updating

The owner updates from Settings in the UI. The container install takes the image to switch to and replaces its own container. The binary install takes the URL of the new binary, checks it against its `.sha256`, runs its migrations and only then swaps it in. The old binary is kept as `kuroe.prev`.

Running the installer again also updates.

## Not tested yet

These work in tests but haven't been run against the real services:

- Let's Encrypt certificates (needs a public domain).
- GitHub, GitLab, Gitea and Cloudflare API calls, GitHub App setup and pulls from private registries.
- Self-hosted CI runners registering with GitHub, GitLab, Gitea or Forgejo, and the generated pipelines running there.
- kuroe's Traefik behind a real nginx, Apache, Caddy, HAProxy or lighttpd.
- Clean installs on Ubuntu and on macOS Docker Desktop.

## Building from source

You need Node 26, Rust 1.99 and Docker.

```sh
cd web && npm ci && npm run build && cd ..
cargo build --release --locked
```

The web build has to come first, because the binary embeds `web/dist`. `docker build -t kuroe .` does both.

Tests need the Docker socket:

```sh
cd web && npm test && cd ..
cargo test --locked
```

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
