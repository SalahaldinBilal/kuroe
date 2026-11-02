# Changelog

## [Unreleased]

- The web server settings on the Proxy page are clearer. Who keeps the HTTPS certificates is a plain choice, and the certificate paths say that kuroe doesn't create those files.
- When kuroe runs in a container and the web server runs on the server itself, the Proxy page says kuroe can't install the config there and shows what to copy and run instead. Installing in that setup is refused with the same explanation instead of failing with "nginx: not found".
- Copy buttons work when kuroe is opened over plain HTTP.
- Config previews show the whole file, and the Copy button no longer covers it.
- Segmented controls no longer stretch into an empty bar when placed in a form field.

## [0.1.0] - 2026-11-02

First release.