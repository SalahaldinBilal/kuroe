import "@xterm/xterm/css/xterm.css";
import { AttachAddon } from "@xterm/addon-attach";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal as XTerm } from "@xterm/xterm";
import { createSignal, onCleanup, onMount, Show } from "solid-js";

interface Props {
  url: string;
  interactive: boolean;
  onResize?: (cols: number, rows: number) => void;
}

function token(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

export function Terminal(props: Props) {
  let host!: HTMLDivElement;
  const [closed, setClosed] = createSignal(false);

  onMount(() => {
    const term = new XTerm({
      fontFamily: token("--font-mono"),
      fontSize: 13,
      convertEol: !props.interactive,
      disableStdin: !props.interactive,
      cursorBlink: props.interactive,
      scrollback: 5000,
      theme: {
        background: token("--term-bg"),
        foreground: token("--term-text"),
        cursor: token("--term-text"),
        selectionBackground: "#3a3c40",
      },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    fit.fit();

    const socket = new WebSocket(props.url);
    socket.binaryType = "arraybuffer";
    socket.addEventListener("open", () => props.onResize?.(term.cols, term.rows));
    socket.addEventListener("close", () => setClosed(true));
    term.loadAddon(new AttachAddon(socket, { bidirectional: props.interactive }));
    term.onResize(({ cols, rows }) => props.onResize?.(cols, rows));
    if (props.interactive) term.focus();

    let frame = 0;
    let lastSize = "";
    const observer = new ResizeObserver(([entry]) => {
      const { width, height } = entry.contentRect;
      const size = `${Math.round(width)}x${Math.round(height)}`;
      if (!width || !height || size === lastSize) return;
      lastSize = size;
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => fit.fit());
    });
    observer.observe(host);

    onCleanup(() => {
      cancelAnimationFrame(frame);
      observer.disconnect();
      socket.close();
      term.dispose();
    });
  });

  return (
    <div class="xterm-box">
      <div class="xterm-host" ref={host} />
      <Show when={closed()}>
        <p class="closed">Connection closed</p>
      </Show>
    </div>
  );
}
