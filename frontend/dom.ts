export function node<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  value?: string | null,
  className?: string,
) {
  const item = document.createElement(tag);
  if (value !== undefined) item.textContent = value;
  if (className) item.className = className;
  return item;
}

export function svg(name: string) {
  const ns = "http://www.w3.org/2000/svg";
  const icon = document.createElementNS(ns, "svg");
  const use = document.createElementNS(ns, "use");
  use.setAttribute("href", "#" + name);
  icon.setAttribute("aria-hidden", "true");
  icon.append(use);
  return icon;
}
