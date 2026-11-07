# Changelog

## [Unreleased]

- "Add it for me" on the push webhook no longer turns the webhook on when the provider refuses it. Before, kuroe saved the new secret first, so the card showed "Enabled" after a refresh even though no hook existed at GitHub, GitLab or Gitea. Refusals are now also written to kuroe's log.
- kuroe's errors reach the browser behind Cloudflare. Errors about Docker, the proxy, providers and the update check were sent as 502 or 504, and Cloudflare replaced those with its own "Bad gateway" page. They now come back as 503 with kuroe's message.

## [0.2.2] - 2026-11-06

- Updating no longer rolls back on servers with HTTP access set to "local". The new version started fine, but its check came over plain HTTP from the updater container and got refused, so the update timed out after 30 seconds. The check now uses `/api/v1/health`. With "local", that route also answers plain HTTP sent straight from a container on the same host, and nothing else from outside.

## [0.2.1] - 2026-11-06

- Build output keeps the colors the build tools print. Docker's classic builder painted everything written to stderr bright red, so normal progress like cargo's "Updating crates.io index" looked like errors.
- Logs keep being saved after a container restarts. Before, a restart could stop kuroe from saving that container's logs until kuroe itself restarted.
- Saved logs of the current day add new lines at the end instead of reloading everything when a container starts or stops.
- Saved logs show only each line's message. The time sits in a column on the left, with the full time, stream and container on hover. Deployments with more than one container also show which container wrote each line. Switches above the log turn the time, stream and container columns on or off, and the browser remembers the choice.
- Charts no longer fade out while they reload in the background. The "By deployment" charts on the Metrics page faded every 10 seconds, and a deployment's charts faded whenever one of its containers started or stopped. A live range now shows "Live" next to the range picker, and "Refreshing" while new samples load.
- Clicking a name in a chart's legend hides that line, and the scale adjusts to the lines still shown. Click it again to bring it back.
- Environment variable and secret values mark their spaces. Spaces inside a value get a light tint, and spaces at the start or end get a yellow one. A value that starts or ends with spaces, tabs or a non-breaking space says so under the field, with a link to remove them. Secret values, and the value field on the Secrets page, show that note too.
- Errors that come from a proxy in front of kuroe, or from kuroe's "HTTPS only" refusal, say what happened instead of showing an empty message. Buttons like "Add it for me" showed only an icon when that happened.
- The CPU demand chart's title is just "CPU demand", without "(load average)" or the "all cores busy" note, which broke the value and the time over several lines. The line still shows once the load gets near the core count.
- The Domains table keeps space between a domain's target and its routing summary, and between the certificate picker and the certificate name.
- The update card in Settings has one button for a new release, "Update to 0.3.0", instead of filling the image field first. While kuroe updates, the card only shows the progress and the time so far. Updating to a different image or binary is under "Update to another version", and the release check runs again once the update is done.
- Each step of a deploy opens to show the output it wrote, for a running deploy and for finished ones. Deploys from before this version only show their full output.

## [0.2.0] - 2026-11-03

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