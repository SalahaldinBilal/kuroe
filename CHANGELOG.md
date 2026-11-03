# Changelog

## [Unreleased]

- Live updates, logs, shells and deploy output work again when plain HTTP is set to local only. Traefik marks websockets over HTTPS as `wss`, which kuroe refused as plain HTTP.
- Links inside text, like "Release notes" on the update card, are underlined, and links that open a new tab show an arrow.
- Selected text is easy to see in dark mode, in inputs, on pages, in deploy output and in terminals.
- The selected option in full-width segmented controls, like "Sends traffic to" in Add domain, fills the control's height and sits centered.
- A Git repository deployed with its Compose file no longer needs the file's path. Left empty, kuroe uses `compose.yaml`, `compose.yml`, `docker-compose.yaml` or `docker-compose.yml` from the repository root, the same files Docker looks for.
- New deployment shows the environment variables for Compose deployments too, a pasted Compose file or one from a repository. They fill the variables in the Compose file, as they already did when set in the deployment settings.
- A deployment can allow access to this machine. Its containers then reach the server at `host.docker.internal`, in Compose projects and Swarm stacks. It's off by default and is kept in exports.

## [0.1.1] - 2026-11-02

- The web server settings on the Proxy page are clearer. Who keeps the HTTPS certificates is a plain choice, and the certificate paths say that kuroe doesn't create those files.
- When kuroe runs in a container and the web server runs on the server itself, the Proxy page says kuroe can't install the config there and shows what to copy and run instead. Installing in that setup is refused with the same explanation instead of failing with "nginx: not found".
- Copy buttons work when kuroe is opened over plain HTTP.
- Config previews show the whole file, and the Copy button no longer covers it.
- Segmented controls no longer stretch into an empty bar when placed in a form field.

## [0.1.0] - 2026-11-02

First release.