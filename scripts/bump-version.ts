import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const version = process.argv[2];

if (!version || !/^\d+\.\d+\.\d+$/.test(version)) {
  console.error("Usage: npm run version:bump <version>  (e.g. 0.2.0)");
  process.exit(1);
}

function absolute(relativePath: string): string {
  return fileURLToPath(new URL(relativePath, import.meta.url));
}

function replaceVersion(relativePath: string, pattern: RegExp, replacement: string) {
  const filePath = absolute(relativePath);
  const contents = readFileSync(filePath, "utf-8");
  if (!pattern.test(contents)) {
    throw new Error(`Could not find a version matching ${pattern} in ${relativePath}`);
  }
  writeFileSync(filePath, contents.replace(pattern, replacement));
  console.log(`Updated ${relativePath}`);
}

function cutChangelog() {
  const relativePath = "../CHANGELOG.md";
  const filePath = absolute(relativePath);
  const contents = readFileSync(filePath, "utf-8");

  if (contents.includes(`## [${version}]`)) {
    throw new Error(`${relativePath} already has a section for ${version}`);
  }

  const unreleasedHeading = "## [Unreleased]";
  const headingStart = contents.indexOf(unreleasedHeading);
  if (headingStart === -1) {
    throw new Error(`Could not find "${unreleasedHeading}" in ${relativePath}`);
  }

  const sectionStart = headingStart + unreleasedHeading.length;
  const nextHeading = contents.indexOf("\n## [", sectionStart);
  const sectionEnd = nextHeading === -1 ? contents.length : nextHeading;
  const sectionBody = contents.slice(sectionStart, sectionEnd).trim();

  if (!sectionBody) {
    console.warn(`Warning: the "${unreleasedHeading}" section in ${relativePath} is empty, so this release has no notes.`);
  }

  const date = new Date().toISOString().slice(0, 10);
  const releasedHeading = `## [${version}] - ${date}`;
  const replacement = sectionBody
    ? `${unreleasedHeading}\n\n${releasedHeading}\n\n${sectionBody}\n`
    : `${unreleasedHeading}\n\n${releasedHeading}\n`;

  writeFileSync(filePath, contents.slice(0, headingStart) + replacement + contents.slice(sectionEnd));
  console.log(`Updated ${relativePath}`);
}

replaceVersion("../Cargo.toml", /^version = "[^"]*"/m, `version = "${version}"`);
replaceVersion("../Cargo.lock", /(name = "kuroe"\r?\nversion = )"[^"]*"/, `$1"${version}"`);
cutChangelog();

console.log(`\nBumped to ${version}. Commit, then push the tag v${version} to release.`);
