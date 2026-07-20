// @vitest-environment jsdom

import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { MarkdownContent } from "./MarkdownContent";

afterEach(cleanup);

describe("MarkdownContent", () => {
  it("renders GFM without executing raw HTML or unsafe links", () => {
    const { container } = render(
      <MarkdownContent>{[
        "**bold** | value",
        "--- | ---",
        "row | ok",
        "<script>globalThis.compromised = true</script>",
        "[unsafe](javascript:alert(1)) [safe](https://example.com)",
      ].join("\n")}</MarkdownContent>,
    );
    expect(screen.getByText("bold").tagName).toBe("STRONG");
    expect(container.querySelector("script")).toBeNull();
    expect(screen.getByRole("link", { name: "safe" }).getAttribute("href")).toBe("https://example.com");
    expect(screen.queryByRole("link", { name: "unsafe" })).toBeNull();
  });
});
