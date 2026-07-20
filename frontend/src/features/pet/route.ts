export function isPetWindowRoute(search = window.location.search): boolean {
  return new URLSearchParams(search).get("window") === "pet";
}
