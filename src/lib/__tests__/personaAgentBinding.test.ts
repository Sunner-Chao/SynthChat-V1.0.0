import { describe, it, expect } from "vitest";
import {
  resolvePersonaBoundAgent,
  resolvePersonaAgentBinding,
} from "../personaAgentBinding";
import type { AgentDefinition, LlmProvider, Persona } from "../types";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const defaultAgent: AgentDefinition = {
  id: "agent-default",
  name: "Default Agent",
  isDefault: true,
} as AgentDefinition;

const specificAgent: AgentDefinition = {
  id: "agent-specific",
  name: "Specific Agent",
  isDefault: false,
} as AgentDefinition;

const agents = [specificAgent, defaultAgent];

const enabledProvider: LlmProvider = {
  id: "provider-gpt",
  name: "GPT Provider",
  enabled: true,
} as LlmProvider;

const disabledProvider: LlmProvider = {
  id: "provider-disabled",
  name: "Disabled Provider",
  enabled: false,
} as LlmProvider;

const providers = [enabledProvider, disabledProvider];

const persona: Persona = {
  id: "persona-1",
  name: "Alice",
  agentId: "agent-specific",
  llmProvider: "provider-gpt",
  llmModel: "gpt-4o",
} as Persona;

// ---------------------------------------------------------------------------
// resolvePersonaBoundAgent
// ---------------------------------------------------------------------------

describe("resolvePersonaBoundAgent", () => {
  it("resolves the agent specified on the persona", () => {
    const result = resolvePersonaBoundAgent(persona, agents);
    expect(result?.id).toBe("agent-specific");
  });

  it("falls back to fallbackAgentId when persona has no agentId", () => {
    const noAgent = { ...persona, agentId: "" };
    const result = resolvePersonaBoundAgent(noAgent, agents, "agent-specific");
    expect(result?.id).toBe("agent-specific");
  });

  it("falls back to default agent when nothing matches", () => {
    const noAgent = { ...persona, agentId: "nonexistent" };
    const result = resolvePersonaBoundAgent(noAgent, agents);
    expect(result?.id).toBe("agent-default");
  });

  it("returns first agent when no default exists", () => {
    const noDefault = agents.map((a) => ({ ...a, isDefault: false }));
    const result = resolvePersonaBoundAgent(null, noDefault);
    expect(result?.id).toBe(noDefault[0].id);
  });

  it("returns null when agents list is empty", () => {
    const result = resolvePersonaBoundAgent(persona, []);
    expect(result).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// resolvePersonaAgentBinding
// ---------------------------------------------------------------------------

describe("resolvePersonaAgentBinding", () => {
  it("resolves a fully configured persona binding", () => {
    const binding = resolvePersonaAgentBinding(persona, agents, providers);
    expect(binding.agent?.id).toBe("agent-specific");
    expect(binding.provider?.id).toBe("provider-gpt");
    expect(binding.model).toBe("gpt-4o");
    expect(binding.providerDisabled).toBe(false);
    expect(binding.infoText).toContain("GPT Provider");
  });

  it("marks provider as disabled when provider is disabled", () => {
    const withDisabled = {
      ...persona,
      llmProvider: "provider-disabled",
    };
    const binding = resolvePersonaAgentBinding(withDisabled, agents, providers);
    expect(binding.providerDisabled).toBe(true);
    expect(binding.infoText).toBe("服务商已停用");
  });

  it("shows 请选择通讯录服务商 when no provider configured", () => {
    const noProvider = { ...persona, llmProvider: "" };
    const binding = resolvePersonaAgentBinding(noProvider, agents, providers);
    expect(binding.infoText).toBe("请选择通讯录服务商");
    expect(binding.providerDisabled).toBe(false);
  });

  it("marks provider as disabled when provider id is unknown (providerMissing)", () => {
    const unknownProvider = { ...persona, llmProvider: "does-not-exist" };
    const binding = resolvePersonaAgentBinding(unknownProvider, agents, []);
    // providerMissing = true because personaProviderId is set but not found
    expect(binding.providerDisabled).toBe(true);
    expect(binding.infoText).toBe("服务商已停用");
  });

  it("populates searchText with relevant fields", () => {
    const binding = resolvePersonaAgentBinding(persona, agents, providers);
    expect(binding.searchText).toContain("alice");
    expect(binding.searchText).toContain("gpt provider");
    expect(binding.searchText).toContain("gpt-4o");
  });

  it("handles null persona gracefully", () => {
    const binding = resolvePersonaAgentBinding(null, agents, providers);
    expect(binding.agent).not.toBeNull();
    expect(binding.provider).toBeNull();
    expect(binding.providerDisabled).toBe(false);
  });
});
