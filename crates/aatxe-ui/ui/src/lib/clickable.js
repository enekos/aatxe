// Make a non-interactive element (our nav <li>s) behave like a button:
// pointer click + Enter/Space activation + the right ARIA role/focus. Applied
// via `use:` so there's no inline handler for the compiler to flag a11y on.
export function clickable(node, handler) {
  let onActivate = handler;
  node.setAttribute("role", "button");
  node.setAttribute("tabindex", "0");
  const onClick = () => onActivate();
  const onKey = (e) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onActivate();
    }
  };
  node.addEventListener("click", onClick);
  node.addEventListener("keydown", onKey);
  return {
    update(next) { onActivate = next; },
    destroy() {
      node.removeEventListener("click", onClick);
      node.removeEventListener("keydown", onKey);
    },
  };
}
