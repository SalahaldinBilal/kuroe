import type { DeploySource, DeployStep, DeployStepName, DeploySummary } from "./api";

export const STEP_LABELS: Record<DeployStepName, string> = {
  resolving_secrets: "Resolving secrets",
  cloning: "Cloning",
  checking_pool: "Checking the pool",
  building: "Building",
  pulling: "Pulling",
  starting: "Starting",
  health_check: "Health check",
  switching_traffic: "Switching traffic",
  draining: "Draining the old version",
  stopping_old: "Stopping the old version",
  publishing_routes: "Publishing routes",
  installing_files: "Installing files",
  restoring_routes: "Restoring the routes",
  stopping_new: "Stopping the new version",
  cleaning_up: "Cleaning up",
};

export function stepText(step: DeployStepName, detail?: string | null): string {
  return detail ? `${STEP_LABELS[step]} ${detail}` : STEP_LABELS[step];
}

/** The commit page when the server knows the provider, or null. */
export function commitUrl(source: DeploySource): string | null {
  const { url, commit, provider } = source;
  if (!url || !commit || !provider || !url.startsWith("https://")) return null;
  const base = url.replace(/\/+$/, "").replace(/\.git$/, "");
  return provider === "gitlab" ? `${base}/-/commit/${commit}` : `${base}/commit/${commit}`;
}

export function sourceText(source: DeploySource): string {
  if (source.commit) return source.commit.slice(0, 7);
  if (source.image) return source.digest ? `${source.image}@${source.digest.slice(7, 19)}` : source.image;
  if (source.reference) return source.reference;
  return source.kind;
}

export function triggerText(deploy: DeploySummary): string {
  return deploy.trigger === "deploy_token" ? `token ${deploy.triggered_by}` : deploy.triggered_by;
}

/** How long a step took, or has taken so far. Zero for one still to come. */
export function stepSeconds(step: DeployStep, nowMs: number): number {
  if (step.started_ms == null) return 0;
  return ((step.finished_ms ?? nowMs) - step.started_ms) / 1000;
}

export function deploySeconds(deploy: DeploySummary, nowSeconds: number): number {
  return (deploy.finished_at ?? nowSeconds) - deploy.started_at;
}
