import Button from "./ui/Button/Button";
import EmptyState from "./ui/EmptyState/EmptyState";
import { Icon } from "./ui/Icon";

export function StartupFailed(props: { message: string; onRetry: () => void }) {
  return (
    <div class="center">
      <section class="card" style={{ width: "min(560px, 100%)" }}>
        <EmptyState
          icon="alert"
          tone="danger"
          title="kuroe couldn't start"
          description="Fix the problem below, then restart kuroe. The server log has the same message."
          actions={
            <Button onClick={props.onRetry}>
              <Icon name="rotate" />
              Check again
            </Button>
          }
        />
        <div class="card-b" style={{ "border-top": "1px solid var(--line-soft)" }}>
          <pre class="code" style={{ "white-space": "pre-wrap", "text-align": "left", margin: "0" }}>
            {props.message}
          </pre>
        </div>
      </section>
    </div>
  );
}
