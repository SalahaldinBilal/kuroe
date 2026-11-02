import { createResource, createRoot, createSignal } from "solid-js";
import { api } from "../api";

// One list for every editor on the page. It loads the first time someone who may use saved secrets opens one.
const state = createRoot(() => {
  const [wanted, setWanted] = createSignal(false);
  const [list, { refetch }] = createResource(wanted, () => api.savedSecrets());
  return { list, refetch, load: () => setWanted(true) };
});

export default state;
