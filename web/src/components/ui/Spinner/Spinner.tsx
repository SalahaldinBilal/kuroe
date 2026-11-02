import styles from "./Spinner.module.scss";

function Spinner(props: { size?: number }) {
  const size = () => props.size ?? 16;
  return (
    <svg
      class={styles.Spinner}
      width={size()}
      height={size()}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      aria-hidden="true"
    >
      <path d="M21 12a9 9 0 1 1-6.219-8.56" />
    </svg>
  );
}

export default Spinner;
