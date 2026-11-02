import "@fontsource/ibm-plex-sans/400.css";
import "@fontsource/ibm-plex-sans/500.css";
import "@fontsource/ibm-plex-sans/600.css";
import "@fontsource/ibm-plex-mono/400.css";
import "./styles/app.scss";
import { render } from "solid-js/web";
import { App } from "./App";

const root = document.getElementById("root");
if (root) render(() => <App />, root);
