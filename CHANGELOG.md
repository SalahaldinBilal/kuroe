# Changelog

## [Unreleased]

- Live updates, logs, shells and deploy output work again when plain HTTP is set to local only. Traefik marks websockets over HTTPS as `wss`, which kuroe refused as plain HTTP.
- Links inside text, like "Release notes" on the update card, are underlined, and links that open a new tab show an arrow.
- Selected text is easy to see in dark mode, in inputs, on pages, in deploy output and in terminals.
- The selected option in full-width segmented controls, like "Sends traffic to" in Add domain, fills the control's height and sits centered.
- A Git repository deployed with its Compose file no longer needs the file's path. Left empty, kuroe uses `compose.yaml`, `compose.yml`, `docker-compose.yaml` or `docker-compose.yml` from the repository root, the same files Docker looks for.
- New deployment shows the environment variables for Compose deployments too, a pasted Compose file or one from a repository. They fill the variables in the Compose file, as they already did when set in the deployment settings.
- A deployment can allow access to this machine. Its containers then reach the server at `host.docker.internal`, in Compose projects and Swarm stacks. It's off by default and is kept in exports.
- Deploy output and saved logs show terminal colors instead of codes like `[91m`, and build output no longer puts color codes on lines of their own. Lines redrawn in place, like progress bars, show their last state.
- Ctrl+C (Cmd+C on a Mac) copies the selected text in live logs and shells. Ctrl+Shift+C copies too. In a shell, Ctrl+C with nothing selected still stops the running command.
- Charts scale to their data instead of a far-off limit, and leave little empty space above the highest value. Memory and traffic charts step in powers of two, so a 512 MiB limit lands on a gridline. A limit far above the data is named next to the title. A container without a memory limit no longer shows the host's memory as its limit.
- A deployment's metrics can show each container as its own line, and the Metrics page shows every deployment you can see side by side. The seven busiest get their own line and the rest are added up as "other".
- New deployment lists the repositories a saved GitHub token, GitLab token or Gitea/Forgejo token can read, as it already did for a GitHub App. Type to filter the list, or pick "Another repository" to type a URL.
- Volumes show their size, measured every 10 minutes, on the Volumes page and on the deployments that use them. A deployment's own Compose volumes are listed too.
- A volume can have a size limit. Docker can't cap a volume, so kuroe compares the measured size with it, marks a volume that's over, and can stop the deployments that use it until there's room again.
- A running deploy can be cancelled. kuroe stops the build or start where it is and takes down the half-started version. With blue/green deploys the version from before keeps serving, and the output says who cancelled it.
- Deploy output shows live while the deploy runs, line by line as Docker prints it, with progress bars updating in place. Next to it are the deploy's steps: done ones with how long they took, the current one, and the ones still to come. A failed deploy marks the step that failed.
- The Deployments list shows the step a running deploy is in.
- Images are pulled in their own step before the containers start, so a slow pull no longer looks like a slow start. Images already on the server aren't pulled again, as before.
- A deployment's new Deploys tab lists its deploys: revision, result, who or what started it, the commit or image digest it deployed, when, and how long it took. The commit links to GitHub, GitLab or Gitea. Each deploy opens with its steps and full output. The last 50 are kept per deployment, which can be changed under Settings, Retention.
- Secret values, saved secrets, Git tokens and registry passwords are masked in deploy output, and Git errors no longer quote the clone URL with its password.
- A deploy that was running when kuroe stopped is marked failed on the next start, instead of leaving the deployment stuck as deploying.
- A deployment has a Preview page for sending requests to its containers. Pick a container and one of its ports, or type any other port, then set the method, path, query, headers and a JSON, form or raw body. The answer shows its status, time, size and headers. Pages render without scripts or as source, JSON as a tree, images, video, audio and PDFs inline, text with colors and line numbers, and anything else as hex with a download. Redirects show as they are instead of being followed. Recent requests are kept in the browser tab. The port chips on the Containers card open this page, and "Open in a new tab" still browses through kuroe as before.

## [0.1.1] - 2026-11-02

- The web server settings on the Proxy page are clearer. Who keeps the HTTPS certificates is a plain choice, and the certificate paths say that kuroe doesn't create those files.
- When kuroe runs in a container and the web server runs on the server itself, the Proxy page says kuroe can't install the config there and shows what to copy and run instead. Installing in that setup is refused with the same explanation instead of failing with "nginx: not found".
- Copy buttons work when kuroe is opened over plain HTTP.
- Config previews show the whole file, and the Copy button no longer covers it.
- Segmented controls no longer stretch into an empty bar when placed in a form field.

## [0.1.0] - 2026-11-02

First release.