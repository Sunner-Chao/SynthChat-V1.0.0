import { describe, it, expect } from "vitest";
import { filterSkillsByQuery } from "../skillSearch";

const skills = [
  { id: "web-search", name: "Web Search", description: "Search the web for information" },
  { id: "image-gen", name: "Image Generation", description: "Generate images with AI", author: "anthropic" },
  { id: "code-review", name: "Code Review", description: "Review code quality", version: "1.2.0" },
  { id: "translation", name: "Translate", description: "Translate text between languages" },
];

describe("filterSkillsByQuery", () => {
  it("returns all skills when query is empty", () => {
    expect(filterSkillsByQuery(skills, "")).toHaveLength(4);
    expect(filterSkillsByQuery(skills, "   ")).toHaveLength(4);
  });

  it("matches by skill name (case-insensitive)", () => {
    const results = filterSkillsByQuery(skills, "web");
    expect(results).toHaveLength(1);
    expect(results[0].id).toBe("web-search");
  });

  it("matches by description", () => {
    const results = filterSkillsByQuery(skills, "generate images");
    expect(results).toHaveLength(1);
    expect(results[0].id).toBe("image-gen");
  });

  it("matches by id", () => {
    const results = filterSkillsByQuery(skills, "code-review");
    expect(results).toHaveLength(1);
    expect(results[0].id).toBe("code-review");
  });

  it("matches by author field", () => {
    const results = filterSkillsByQuery(skills, "anthropic");
    expect(results).toHaveLength(1);
    expect(results[0].id).toBe("image-gen");
  });

  it("matches by version field", () => {
    const results = filterSkillsByQuery(skills, "1.2.0");
    expect(results).toHaveLength(1);
    expect(results[0].id).toBe("code-review");
  });

  it("returns empty array when nothing matches", () => {
    expect(filterSkillsByQuery(skills, "xxxxxxxxxx")).toHaveLength(0);
  });

  it("handles partial matches across multiple skills", () => {
    const results = filterSkillsByQuery(skills, "text");
    expect(results).toHaveLength(1);
    expect(results[0].id).toBe("translation");
  });

  it("is case-insensitive", () => {
    const lower = filterSkillsByQuery(skills, "web search");
    const upper = filterSkillsByQuery(skills, "WEB SEARCH");
    const mixed = filterSkillsByQuery(skills, "Web Search");
    expect(lower).toEqual(upper);
    expect(lower).toEqual(mixed);
  });
});
