export function Logo(props: { size?: number }) {
  return (
    <svg
      class="logo"
      width={props.size ?? 20}
      height={props.size ?? 20}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="square"
      stroke-linejoin="miter"
      aria-hidden="true"
    >
      <path d="M8 3v18" />
      <g fill="currentColor" stroke="none">
        <path d="M7 4.5L4.2 3.6 5.1 5.3 3.6 6.4 7 7.3z" />
        <path d="M7 9L2.4 7.6 3.9 10 1.8 11.5 7 13.2z" />
        <path d="M7 15.4L3.6 14.6 4.6 16.4 3 17.6 7 18.6z" />
      </g>
      <path d="M20.5 3L12 12l8.5 9" />
    </svg>
  );
}
