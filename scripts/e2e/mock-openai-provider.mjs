import { createHash, timingSafeEqual } from "node:crypto";
import { createServer } from "node:http";
import { isIP } from "node:net";

function requiredEnvironment(name) {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`${name} is required.`);
  return value;
}

function optionalEnvironment(name) {
  const value = process.env[name]?.trim();
  return value || null;
}

function nonNegativeInteger(name, fallback) {
  const raw = process.env[name]?.trim();
  if (!raw) return fallback;
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`${name} must be a non-negative integer.`);
  }
  return value;
}

function positiveInteger(name, fallback) {
  const value = nonNegativeInteger(name, fallback);
  if (value <= 0) throw new Error(`${name} must be a positive integer.`);
  return value;
}

function loopbackHost() {
  const host = process.env.SYNTHCHAT_E2E_PROVIDER_HOST?.trim() || "127.0.0.1";
  if (isIP(host) === 0 || !["127.0.0.1", "::1"].includes(host)) {
    throw new Error("SYNTHCHAT_E2E_PROVIDER_HOST must be a loopback IP address.");
  }
  return host;
}

function origin(host, port) {
  return `http://${host.includes(":") ? `[${host}]` : host}:${port}`;
}

function writeEvent(response, value) {
  response.write(`data: ${JSON.stringify(value)}\n\n`);
}

function secureEqual(actual, expected) {
  if (typeof actual !== "string") return false;
  const actualBytes = Buffer.from(actual, "utf8");
  const expectedBytes = Buffer.from(expected, "utf8");
  return actualBytes.length === expectedBytes.length
    && timingSafeEqual(actualBytes, expectedBytes);
}

function jsonResponse(response, status, value) {
  const body = JSON.stringify(value);
  response.writeHead(status, {
    "Cache-Control": "no-store",
    "Content-Length": Buffer.byteLength(body),
    "Content-Type": "application/json; charset=utf-8",
  });
  response.end(body);
}

function requestPath(request) {
  const rawUrl = request.url || "/";
  if (!rawUrl.startsWith("/") || rawUrl.startsWith("//")) return null;
  try {
    return new URL(rawUrl, "http://loopback.invalid");
  } catch {
    return null;
  }
}

async function listen(server, host, port, name) {
  await new Promise((resolveListening, reject) => {
    server.once("error", reject);
    server.listen({ host, port, exclusive: true }, resolveListening);
  });
  const address = server.address();
  if (!address || typeof address === "string") {
    throw new Error(`${name} address is unavailable.`);
  }
  return origin(host, address.port);
}

const host = loopbackHost();
const port = nonNegativeInteger("SYNTHCHAT_E2E_PROVIDER_PORT", 0);
const controlPort = nonNegativeInteger("SYNTHCHAT_E2E_PROVIDER_CONTROL_PORT", 0);
if (port > 65_535 || controlPort > 65_535) {
  throw new Error("Mock provider ports must not exceed 65535.");
}
const routePath = process.env.SYNTHCHAT_E2E_PROVIDER_PATH?.trim() || "/v1/chat/completions";
if (!routePath.startsWith("/") || routePath.includes("?")) {
  throw new Error("SYNTHCHAT_E2E_PROVIDER_PATH must be an absolute path without a query.");
}
const reply = requiredEnvironment("SYNTHCHAT_E2E_REPLY");
const promptTokens = positiveInteger("SYNTHCHAT_E2E_PROMPT_TOKENS", 7);
const completionTokens = positiveInteger("SYNTHCHAT_E2E_COMPLETION_TOKENS", 5);
const totalTokens = positiveInteger(
  "SYNTHCHAT_E2E_TOTAL_TOKENS",
  promptTokens + completionTokens,
);
const approvalPrompt = optionalEnvironment("SYNTHCHAT_E2E_APPROVAL_PROMPT");
const approvalReply = optionalEnvironment("SYNTHCHAT_E2E_APPROVAL_REPLY");
const approvalCallId = optionalEnvironment("SYNTHCHAT_E2E_APPROVAL_CALL_ID");
const approvalReadCallId = optionalEnvironment("SYNTHCHAT_E2E_APPROVAL_READ_CALL_ID");
const approvalSearchCallId = optionalEnvironment("SYNTHCHAT_E2E_APPROVAL_SEARCH_CALL_ID");
const approvalPatchCallId = optionalEnvironment("SYNTHCHAT_E2E_APPROVAL_PATCH_CALL_ID");
const approvalRelativePath = optionalEnvironment("SYNTHCHAT_E2E_APPROVAL_RELATIVE_PATH");
const approvalPublicNeedle = optionalEnvironment("SYNTHCHAT_E2E_APPROVAL_PUBLIC_NEEDLE");
const approvalPrivateContent = optionalEnvironment("SYNTHCHAT_E2E_APPROVAL_PRIVATE_CONTENT");
const approvalPatchedContent = optionalEnvironment("SYNTHCHAT_E2E_APPROVAL_PATCHED_CONTENT");
const approvalValues = [
  approvalPrompt,
  approvalReply,
  approvalCallId,
  approvalReadCallId,
  approvalSearchCallId,
  approvalPatchCallId,
  approvalRelativePath,
  approvalPublicNeedle,
  approvalPrivateContent,
  approvalPatchedContent,
];
if (approvalValues.some(Boolean) && approvalValues.some((value) => value === null)) {
  throw new Error("The scripted approval environment must be configured as one complete set.");
}
const scriptedApproval = approvalPrompt === null ? null : {
  callId: approvalCallId,
  patchCallId: approvalPatchCallId,
  patchedContent: approvalPatchedContent,
  privateContent: approvalPrivateContent,
  prompt: approvalPrompt,
  publicNeedle: approvalPublicNeedle,
  readCallId: approvalReadCallId,
  relativePath: approvalRelativePath,
  reply: approvalReply,
  searchCallId: approvalSearchCallId,
};
if (scriptedApproval) {
  if (new Set([
    scriptedApproval.callId,
    scriptedApproval.readCallId,
    scriptedApproval.searchCallId,
    scriptedApproval.patchCallId,
  ]).size !== 4) {
    throw new Error("The scripted Files call IDs must be distinct.");
  }
  if ([
    scriptedApproval.publicNeedle,
    scriptedApproval.privateContent,
    scriptedApproval.patchedContent,
  ].some((value) => /[\r\n]/u.test(value))
    || !scriptedApproval.privateContent.includes(scriptedApproval.publicNeedle)
    || !scriptedApproval.patchedContent.includes(scriptedApproval.publicNeedle)
    || scriptedApproval.privateContent === scriptedApproval.patchedContent) {
    throw new Error("The scripted Files fixture values do not match their contract.");
  }
}
const clarificationPrompt = optionalEnvironment("SYNTHCHAT_E2E_CLARIFICATION_PROMPT");
const clarificationReply = optionalEnvironment("SYNTHCHAT_E2E_CLARIFICATION_REPLY");
const clarificationCallId = optionalEnvironment("SYNTHCHAT_E2E_CLARIFICATION_CALL_ID");
const clarificationQuestion = optionalEnvironment("SYNTHCHAT_E2E_CLARIFICATION_QUESTION");
const clarificationAnswer = optionalEnvironment("SYNTHCHAT_E2E_CLARIFICATION_ANSWER");
const clarificationValues = [
  clarificationPrompt,
  clarificationReply,
  clarificationCallId,
  clarificationQuestion,
  clarificationAnswer,
];
if (clarificationValues.some(Boolean) && clarificationValues.some((value) => value === null)) {
  throw new Error("The scripted clarification environment must be configured as one complete set.");
}
const scriptedClarification = clarificationPrompt === null ? null : {
  answer: clarificationAnswer,
  callId: clarificationCallId,
  prompt: clarificationPrompt,
  question: clarificationQuestion,
  reply: clarificationReply,
};
const mcpPrompt = optionalEnvironment("SYNTHCHAT_E2E_MCP_PROMPT");
const mcpReply = optionalEnvironment("SYNTHCHAT_E2E_MCP_REPLY");
const mcpCallId = optionalEnvironment("SYNTHCHAT_E2E_MCP_CALL_ID");
const mcpToolName = optionalEnvironment("SYNTHCHAT_E2E_MCP_TOOL_NAME");
const mcpPrivateResult = optionalEnvironment("SYNTHCHAT_E2E_MCP_PRIVATE_RESULT");
const mcpValues = [mcpPrompt, mcpReply, mcpCallId, mcpToolName, mcpPrivateResult];
if (mcpValues.some(Boolean) && mcpValues.some((value) => value === null)) {
  throw new Error("The scripted MCP environment must be configured as one complete set.");
}
const scriptedMcp = mcpPrompt === null ? null : {
  callId: mcpCallId,
  privateResult: mcpPrivateResult,
  prompt: mcpPrompt,
  reply: mcpReply,
  toolName: mcpToolName,
};
const terminalPrompt = optionalEnvironment("SYNTHCHAT_E2E_TERMINAL_PROMPT");
const terminalReply = optionalEnvironment("SYNTHCHAT_E2E_TERMINAL_REPLY");
const terminalCallId = optionalEnvironment("SYNTHCHAT_E2E_TERMINAL_CALL_ID");
const terminalCommand = optionalEnvironment("SYNTHCHAT_E2E_TERMINAL_COMMAND");
const terminalPrivateOutput = optionalEnvironment("SYNTHCHAT_E2E_TERMINAL_PRIVATE_OUTPUT");
const terminalValues = [
  terminalPrompt,
  terminalReply,
  terminalCallId,
  terminalCommand,
  terminalPrivateOutput,
];
if (terminalValues.some(Boolean) && terminalValues.some((value) => value === null)) {
  throw new Error("The scripted terminal environment must be configured as one complete set.");
}
const scriptedTerminal = terminalPrompt === null ? null : {
  callId: terminalCallId,
  command: terminalCommand,
  privateOutput: terminalPrivateOutput,
  prompt: terminalPrompt,
  reply: terminalReply,
};
const backgroundTerminalPrompt = optionalEnvironment("SYNTHCHAT_E2E_BACKGROUND_TERMINAL_PROMPT");
const backgroundTerminalReply = optionalEnvironment("SYNTHCHAT_E2E_BACKGROUND_TERMINAL_REPLY");
const backgroundProcessPrompt = optionalEnvironment("SYNTHCHAT_E2E_BACKGROUND_PROCESS_PROMPT");
const backgroundProcessReply = optionalEnvironment("SYNTHCHAT_E2E_BACKGROUND_PROCESS_REPLY");
const backgroundTerminalCallId = optionalEnvironment(
  "SYNTHCHAT_E2E_BACKGROUND_TERMINAL_CALL_ID",
);
const backgroundProcessListCallId = optionalEnvironment(
  "SYNTHCHAT_E2E_BACKGROUND_PROCESS_LIST_CALL_ID",
);
const backgroundProcessKillCallId = optionalEnvironment(
  "SYNTHCHAT_E2E_BACKGROUND_PROCESS_KILL_CALL_ID",
);
const backgroundTerminalCommand = optionalEnvironment(
  "SYNTHCHAT_E2E_BACKGROUND_TERMINAL_COMMAND",
);
const backgroundTerminalPrivateOutput = optionalEnvironment(
  "SYNTHCHAT_E2E_BACKGROUND_TERMINAL_PRIVATE_OUTPUT",
);
const backgroundTerminalValues = [
  backgroundTerminalPrompt,
  backgroundTerminalReply,
  backgroundProcessPrompt,
  backgroundProcessReply,
  backgroundTerminalCallId,
  backgroundProcessListCallId,
  backgroundProcessKillCallId,
  backgroundTerminalCommand,
  backgroundTerminalPrivateOutput,
];
if (backgroundTerminalValues.some(Boolean)
  && backgroundTerminalValues.some((value) => value === null)) {
  throw new Error(
    "The scripted background terminal environment must be configured as one complete set.",
  );
}
const scriptedBackgroundTerminal = backgroundTerminalPrompt === null ? null : {
  command: backgroundTerminalCommand,
  commandPreview: `command sha256:${createHash("sha256")
    .update(backgroundTerminalCommand, "utf8")
    .digest("hex")
    .slice(0, 12)}`,
  killCallId: backgroundProcessKillCallId,
  listCallId: backgroundProcessListCallId,
  privateOutput: backgroundTerminalPrivateOutput,
  processPrompt: backgroundProcessPrompt,
  processReply: backgroundProcessReply,
  prompt: backgroundTerminalPrompt,
  reply: backgroundTerminalReply,
  terminalCallId: backgroundTerminalCallId,
};
if (scriptedBackgroundTerminal && new Set([
  scriptedBackgroundTerminal.terminalCallId,
  scriptedBackgroundTerminal.listCallId,
  scriptedBackgroundTerminal.killCallId,
]).size !== 3) {
  throw new Error("The scripted background terminal call IDs must be distinct.");
}
if (scriptedBackgroundTerminal
  && scriptedBackgroundTerminal.prompt === scriptedBackgroundTerminal.processPrompt) {
  throw new Error("The scripted background terminal prompts must be distinct.");
}
const codePrompt = optionalEnvironment("SYNTHCHAT_E2E_CODE_PROMPT");
const codeReply = optionalEnvironment("SYNTHCHAT_E2E_CODE_REPLY");
const codeCallId = optionalEnvironment("SYNTHCHAT_E2E_CODE_CALL_ID");
const codeSource = optionalEnvironment("SYNTHCHAT_E2E_CODE_SOURCE");
const codePrivateOutput = optionalEnvironment("SYNTHCHAT_E2E_CODE_PRIVATE_OUTPUT");
const codeValues = [codePrompt, codeReply, codeCallId, codeSource, codePrivateOutput];
if (codeValues.some(Boolean) && codeValues.some((value) => value === null)) {
  throw new Error("The scripted code environment must be configured as one complete set.");
}
const scriptedCode = codePrompt === null ? null : {
  callId: codeCallId,
  privateOutput: codePrivateOutput,
  prompt: codePrompt,
  reply: codeReply,
  source: codeSource,
};
const browserPrompt = optionalEnvironment("SYNTHCHAT_E2E_BROWSER_PROMPT");
const browserReply = optionalEnvironment("SYNTHCHAT_E2E_BROWSER_REPLY");
const browserNavigateCallId = optionalEnvironment("SYNTHCHAT_E2E_BROWSER_NAVIGATE_CALL_ID");
const browserSnapshotCallId = optionalEnvironment("SYNTHCHAT_E2E_BROWSER_SNAPSHOT_CALL_ID");
const browserCdpCallId = optionalEnvironment("SYNTHCHAT_E2E_BROWSER_CDP_CALL_ID");
const browserPostCdpSnapshotCallId = optionalEnvironment(
  "SYNTHCHAT_E2E_BROWSER_POST_CDP_SNAPSHOT_CALL_ID",
);
const browserDownloadCallId = optionalEnvironment("SYNTHCHAT_E2E_BROWSER_DOWNLOAD_CALL_ID");
const browserDownloadSelector = optionalEnvironment("SYNTHCHAT_E2E_BROWSER_DOWNLOAD_SELECTOR");
const browserDownloadFilename = optionalEnvironment("SYNTHCHAT_E2E_BROWSER_DOWNLOAD_FILENAME");
const browserDownloadPrivateContent = optionalEnvironment(
  "SYNTHCHAT_E2E_BROWSER_DOWNLOAD_PRIVATE_CONTENT",
);
const browserUrl = optionalEnvironment("SYNTHCHAT_E2E_BROWSER_URL");
const browserExpectedTitle = optionalEnvironment("SYNTHCHAT_E2E_BROWSER_EXPECTED_TITLE");
const browserValues = [
  browserPrompt,
  browserReply,
  browserNavigateCallId,
  browserSnapshotCallId,
  browserCdpCallId,
  browserPostCdpSnapshotCallId,
  browserDownloadCallId,
  browserDownloadSelector,
  browserDownloadFilename,
  browserDownloadPrivateContent,
  browserUrl,
  browserExpectedTitle,
];
if (browserValues.some(Boolean) && browserValues.some((value) => value === null)) {
  throw new Error("The scripted Browser environment must be configured as one complete set.");
}
const scriptedBrowser = browserPrompt === null ? null : {
  cdpCallId: browserCdpCallId,
  downloadCallId: browserDownloadCallId,
  downloadFilename: browserDownloadFilename,
  downloadPrivateContent: browserDownloadPrivateContent,
  downloadSelector: browserDownloadSelector,
  downloadSha256: createHash("sha256")
    .update(browserDownloadPrivateContent, "utf8")
    .digest("hex"),
  downloadSizeBytes: Buffer.byteLength(browserDownloadPrivateContent, "utf8"),
  expectedTitle: browserExpectedTitle,
  navigateCallId: browserNavigateCallId,
  postCdpSnapshotCallId: browserPostCdpSnapshotCallId,
  prompt: browserPrompt,
  reply: browserReply,
  snapshotCallId: browserSnapshotCallId,
  url: browserUrl,
};
if (scriptedBrowser) {
  if (!/^#[A-Za-z][A-Za-z0-9_-]{0,127}$/u.test(scriptedBrowser.downloadSelector)) {
    throw new Error("SYNTHCHAT_E2E_BROWSER_DOWNLOAD_SELECTOR must be a bounded ID selector.");
  }
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}\.txt$/u.test(scriptedBrowser.downloadFilename)) {
    throw new Error("SYNTHCHAT_E2E_BROWSER_DOWNLOAD_FILENAME must be a bounded .txt filename.");
  }
  if (new Set([
    scriptedBrowser.navigateCallId,
    scriptedBrowser.snapshotCallId,
    scriptedBrowser.cdpCallId,
    scriptedBrowser.postCdpSnapshotCallId,
    scriptedBrowser.downloadCallId,
  ]).size !== 5) {
    throw new Error("The scripted Browser call IDs must be distinct.");
  }
}
const maxRequestBytes = positiveInteger("SYNTHCHAT_E2E_PROVIDER_MAX_REQUEST_BYTES", 1_048_576);
const controlCapability = requiredEnvironment("SYNTHCHAT_E2E_PROVIDER_CONTROL_CAPABILITY");
if (!/^[0-9a-f]{64}$/u.test(controlCapability)) {
  throw new Error("SYNTHCHAT_E2E_PROVIDER_CONTROL_CAPABILITY must be a 256-bit hex value.");
}
const controlHeader = "x-synthchat-e2e-provider-control";
const midpoint = Math.max(1, Math.floor(reply.length / 2));
let controlState = "idle";
let heldResponse = null;
let requestCount = 0;

function completeResponse(response) {
  if (response.destroyed || response.writableEnded) return;
  writeEvent(response, {
    choices: [{ delta: { content: reply.slice(midpoint) }, finish_reason: "stop", index: 0 }],
  });
  writeEvent(response, {
    choices: [],
    usage: {
      completion_tokens: completionTokens,
      prompt_tokens: promptTokens,
      total_tokens: totalTokens,
    },
  });
  response.end("data: [DONE]\n\n");
}

function completeScriptedApprovalResponse(response, body) {
  if (!scriptedApproval) return false;
  const messages = body.messages;
  const scriptedConversation = messages.some((message) => (
    message?.role === "user" && message?.content === scriptedApproval.prompt
  ));
  if (!scriptedConversation) return false;

  const reject = (message) => {
    response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
    response.end(message);
    return true;
  };
  const exactKeys = (value, keys) => (
    value !== null
    && typeof value === "object"
    && !Array.isArray(value)
    && JSON.stringify(Object.keys(value).sort()) === JSON.stringify([...keys].sort())
  );
  const parseResult = (message, label) => {
    if (typeof message?.content !== "string") throw new Error(`${label} result is absent`);
    return JSON.parse(message.content);
  };

  const lastMessage = messages.at(-1);
  const lastCallId = lastMessage?.role === "tool" ? lastMessage.tool_call_id : null;
  if (lastCallId === null) {
    const definitions = Array.isArray(body.tools) ? body.tools : [];
    const requiredTools = ["write_file", "read_file", "search_files", "patch"];
    if (!requiredTools.every((name) => definitions.some((definition) => (
      definition?.type === "function"
      && definition?.function?.name === name
      && definition?.function?.strict === true
    )))) {
      return reject("scripted Files tools are unavailable");
    }
  }

  let toolCall = null;
  if (lastCallId === null) {
    toolCall = {
      arguments: {
        path: scriptedApproval.relativePath,
        content: scriptedApproval.privateContent,
      },
      id: scriptedApproval.callId,
      name: "write_file",
    };
  } else if (lastCallId === scriptedApproval.callId) {
    let result;
    try {
      result = parseResult(lastMessage, "write_file");
    } catch {
      return reject("scripted write_file result is invalid");
    }
    if (!exactKeys(result, ["path", "bytesWritten", "created"])
      || result.path !== scriptedApproval.relativePath
      || result.bytesWritten !== Buffer.byteLength(scriptedApproval.privateContent, "utf8")
      || result.created !== true
      || JSON.stringify(result).includes(scriptedApproval.privateContent)) {
      return reject("scripted write_file result does not match");
    }
    toolCall = {
      arguments: { path: scriptedApproval.relativePath, offset: 1, limit: 2000 },
      id: scriptedApproval.readCallId,
      name: "read_file",
    };
  } else if (lastCallId === scriptedApproval.readCallId) {
    let result;
    try {
      result = parseResult(lastMessage, "read_file");
    } catch {
      return reject("scripted read_file result is invalid");
    }
    if (!exactKeys(result, [
      "path",
      "content",
      "offset",
      "returnedLines",
      "totalLines",
      "nextOffset",
      "truncated",
    ])
      || result.path !== scriptedApproval.relativePath
      || result.content !== `1|${scriptedApproval.privateContent}\n`
      || result.offset !== 1
      || result.returnedLines !== 1
      || result.totalLines !== 1
      || result.nextOffset !== null
      || result.truncated !== false) {
      return reject("scripted read_file result does not match");
    }
    toolCall = {
      arguments: {
        pattern: scriptedApproval.publicNeedle,
        target: "content",
        path: scriptedApproval.relativePath,
        limit: 10,
        offset: 0,
        output_mode: "content",
        context: 0,
      },
      id: scriptedApproval.searchCallId,
      name: "search_files",
    };
  } else if (lastCallId === scriptedApproval.searchCallId) {
    let result;
    try {
      result = parseResult(lastMessage, "search_files");
    } catch {
      return reject("scripted search_files result is invalid");
    }
    const item = Array.isArray(result?.items) ? result.items[0] : null;
    if (!exactKeys(result, [
      "target",
      "items",
      "offset",
      "returned",
      "nextOffset",
      "truncated",
      "omittedSensitiveFiles",
    ])
      || result.target !== "content"
      || result.items.length !== 1
      || !exactKeys(item, ["path", "line", "text"])
      || item.path !== scriptedApproval.relativePath
      || item.line !== 1
      || item.text !== scriptedApproval.privateContent
      || result.offset !== 0
      || result.returned !== 1
      || result.nextOffset !== null
      || result.truncated !== false
      || result.omittedSensitiveFiles !== 0) {
      return reject("scripted search_files result does not match");
    }
    toolCall = {
      arguments: {
        mode: "replace",
        path: scriptedApproval.relativePath,
        old_string: scriptedApproval.privateContent,
        new_string: scriptedApproval.patchedContent,
        replace_all: false,
      },
      id: scriptedApproval.patchCallId,
      name: "patch",
    };
  } else if (lastCallId === scriptedApproval.patchCallId) {
    let result;
    try {
      result = parseResult(lastMessage, "patch");
    } catch {
      return reject("scripted patch result is invalid");
    }
    if (!exactKeys(result, [
      "success",
      "path",
      "diff",
      "filesModified",
      "replacements",
      "bytesWritten",
    ])
      || result.success !== true
      || result.path !== scriptedApproval.relativePath
      || JSON.stringify(result.filesModified) !== JSON.stringify([scriptedApproval.relativePath])
      || result.replacements !== 1
      || result.bytesWritten !== Buffer.byteLength(scriptedApproval.patchedContent, "utf8")
      || typeof result.diff !== "string"
      || !result.diff.includes(`--- a/${scriptedApproval.relativePath}`)
      || !result.diff.includes(`+++ b/${scriptedApproval.relativePath}`)
      || !result.diff.includes(scriptedApproval.privateContent)
      || !result.diff.includes(scriptedApproval.patchedContent)) {
      return reject("scripted patch result does not match");
    }
  } else {
    return reject("scripted Files continuation is invalid");
  }

  response.writeHead(200, {
    "Cache-Control": "no-cache",
    Connection: "keep-alive",
    "Content-Type": "text/event-stream; charset=utf-8",
  });
  if (toolCall === null) {
    writeEvent(response, {
      choices: [{
        delta: { content: scriptedApproval.reply },
        finish_reason: "stop",
        index: 0,
      }],
      usage: {
        completion_tokens: completionTokens,
        prompt_tokens: promptTokens,
        total_tokens: totalTokens,
      },
    });
  } else {
    writeEvent(response, {
      choices: [{
        delta: {
          tool_calls: [{
            function: {
              arguments: JSON.stringify(toolCall.arguments),
              name: toolCall.name,
            },
            id: toolCall.id,
            index: 0,
            type: "function",
          }],
        },
        finish_reason: "tool_calls",
        index: 0,
      }],
      usage: {
        completion_tokens: completionTokens,
        prompt_tokens: promptTokens,
        total_tokens: totalTokens,
      },
    });
  }
  response.end("data: [DONE]\n\n");
  return true;
}

function completeScriptedClarificationResponse(response, body) {
  if (!scriptedClarification) return false;
  const messages = body.messages;
  const scriptedConversation = messages.some((message) => (
    message?.role === "user" && message?.content === scriptedClarification.prompt
  ));
  if (!scriptedConversation) return false;

  const lastMessage = messages.at(-1);
  const continuingToolCall = lastMessage?.role === "tool"
    && lastMessage?.tool_call_id === scriptedClarification.callId;
  if (continuingToolCall) {
    let result;
    try {
      result = JSON.parse(lastMessage.content);
    } catch {
      response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("scripted clarification result is invalid");
      return true;
    }
    if (result?.answer !== scriptedClarification.answer) {
      response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("scripted clarification answer does not match");
      return true;
    }
  } else {
    const definitions = Array.isArray(body.tools) ? body.tools : [];
    const clarificationAvailable = definitions.some((definition) => (
      definition?.type === "function"
      && definition?.function?.name === "clarify"
    ));
    if (!clarificationAvailable) {
      response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("scripted clarify tool is unavailable");
      return true;
    }
  }

  response.writeHead(200, {
    "Cache-Control": "no-cache",
    Connection: "keep-alive",
    "Content-Type": "text/event-stream; charset=utf-8",
  });
  if (continuingToolCall) {
    writeEvent(response, {
      choices: [{
        delta: { content: scriptedClarification.reply },
        finish_reason: "stop",
        index: 0,
      }],
      usage: {
        completion_tokens: completionTokens,
        prompt_tokens: promptTokens,
        total_tokens: totalTokens,
      },
    });
  } else {
    writeEvent(response, {
      choices: [{
        delta: {
          tool_calls: [{
            function: {
              arguments: JSON.stringify({ question: scriptedClarification.question }),
              name: "clarify",
            },
            id: scriptedClarification.callId,
            index: 0,
            type: "function",
          }],
        },
        finish_reason: "tool_calls",
        index: 0,
      }],
      usage: {
        completion_tokens: completionTokens,
        prompt_tokens: promptTokens,
        total_tokens: totalTokens,
      },
    });
  }
  response.end("data: [DONE]\n\n");
  return true;
}

function completeScriptedMcpResponse(response, body) {
  if (!scriptedMcp) return false;
  const messages = body.messages;
  const scriptedConversation = messages.some((message) => (
    message?.role === "user" && message?.content === scriptedMcp.prompt
  ));
  if (!scriptedConversation) return false;

  const lastMessage = messages.at(-1);
  const continuingToolCall = lastMessage?.role === "tool"
    && lastMessage?.tool_call_id === scriptedMcp.callId;
  if (continuingToolCall) {
    if (typeof lastMessage.content !== "string"
      || !lastMessage.content.includes(scriptedMcp.privateResult)) {
      response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("scripted MCP result does not match");
      return true;
    }
  } else {
    const definitions = Array.isArray(body.tools) ? body.tools : [];
    const mcpToolAvailable = definitions.some((definition) => (
      definition?.type === "function"
      && definition?.function?.name === scriptedMcp.toolName
    ));
    if (!mcpToolAvailable) {
      response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("scripted MCP tool is unavailable");
      return true;
    }
  }

  response.writeHead(200, {
    "Cache-Control": "no-cache",
    Connection: "keep-alive",
    "Content-Type": "text/event-stream; charset=utf-8",
  });
  if (continuingToolCall) {
    writeEvent(response, {
      choices: [{
        delta: { content: scriptedMcp.reply },
        finish_reason: "stop",
        index: 0,
      }],
      usage: {
        completion_tokens: completionTokens,
        prompt_tokens: promptTokens,
        total_tokens: totalTokens,
      },
    });
  } else {
    writeEvent(response, {
      choices: [{
        delta: {
          tool_calls: [{
            function: {
              arguments: JSON.stringify({ text: "hello" }),
              name: scriptedMcp.toolName,
            },
            id: scriptedMcp.callId,
            index: 0,
            type: "function",
          }],
        },
        finish_reason: "tool_calls",
        index: 0,
      }],
      usage: {
        completion_tokens: completionTokens,
        prompt_tokens: promptTokens,
        total_tokens: totalTokens,
      },
    });
  }
  response.end("data: [DONE]\n\n");
  return true;
}

function completeScriptedTerminalResponse(response, body) {
  if (!scriptedTerminal) return false;
  const messages = body.messages;
  const scriptedConversation = messages.some((message) => (
    message?.role === "user" && message?.content === scriptedTerminal.prompt
  ));
  if (!scriptedConversation) return false;

  const lastMessage = messages.at(-1);
  const continuingToolCall = lastMessage?.role === "tool"
    && lastMessage?.tool_call_id === scriptedTerminal.callId;
  if (continuingToolCall) {
    if (typeof lastMessage.content !== "string"
      || !lastMessage.content.includes(scriptedTerminal.privateOutput)) {
      response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("scripted terminal result does not match");
      return true;
    }
  } else {
    const definitions = Array.isArray(body.tools) ? body.tools : [];
    const terminalAvailable = definitions.some((definition) => (
      definition?.type === "function"
      && definition?.function?.name === "terminal"
    ));
    if (!terminalAvailable) {
      response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("scripted terminal tool is unavailable");
      return true;
    }
  }

  response.writeHead(200, {
    "Cache-Control": "no-cache",
    Connection: "keep-alive",
    "Content-Type": "text/event-stream; charset=utf-8",
  });
  if (continuingToolCall) {
    writeEvent(response, {
      choices: [{
        delta: { content: scriptedTerminal.reply },
        finish_reason: "stop",
        index: 0,
      }],
      usage: {
        completion_tokens: completionTokens,
        prompt_tokens: promptTokens,
        total_tokens: totalTokens,
      },
    });
  } else {
    writeEvent(response, {
      choices: [{
        delta: {
          tool_calls: [{
            function: {
              arguments: JSON.stringify({ command: scriptedTerminal.command, timeout: 30 }),
              name: "terminal",
            },
            id: scriptedTerminal.callId,
            index: 0,
            type: "function",
          }],
        },
        finish_reason: "tool_calls",
        index: 0,
      }],
      usage: {
        completion_tokens: completionTokens,
        prompt_tokens: promptTokens,
        total_tokens: totalTokens,
      },
    });
  }
  response.end("data: [DONE]\n\n");
  return true;
}

function rejectScriptedBackgroundTerminal(response, detail) {
  response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
  response.end(detail);
  return true;
}

function backgroundTerminalProcessId(messages) {
  const resultMessage = messages.findLast((message) => (
    message?.role === "tool"
    && message?.tool_call_id === scriptedBackgroundTerminal.terminalCallId
  ));
  if (!resultMessage) return { error: null, processId: null };
  let result;
  try {
    result = JSON.parse(resultMessage.content);
  } catch {
    return { error: "scripted background terminal result is invalid", processId: null };
  }
  if (typeof result?.session_id !== "string"
    || !/^process_[0-9a-f]{32}$/u.test(result.session_id)
    || result.status !== "running") {
    return { error: "scripted background terminal result does not match", processId: null };
  }
  return { error: null, processId: result.session_id };
}

function backgroundListedProcess(messages, terminalProcessId) {
  const resultMessage = messages.findLast((message) => (
    message?.role === "tool"
    && message?.tool_call_id === scriptedBackgroundTerminal.listCallId
  ));
  if (!resultMessage) {
    return { error: "scripted background process list result is missing", processId: null };
  }
  let result;
  try {
    result = JSON.parse(resultMessage.content);
  } catch {
    return { error: "scripted background process list result is invalid", processId: null };
  }
  const matches = Array.isArray(result?.processes)
    ? result.processes.filter((entry) => (
      entry?.status === "running"
      && entry?.command === scriptedBackgroundTerminal.commandPreview
    ))
    : [];
  if (matches.length !== 1) {
    return { error: "scripted background process list match is not unique", processId: null };
  }
  const [match] = matches;
  if (typeof match.session_id !== "string"
    || !/^process_[0-9a-f]{32}$/u.test(match.session_id)
    || (terminalProcessId !== null && match.session_id !== terminalProcessId)
    || JSON.stringify(match).includes(scriptedBackgroundTerminal.privateOutput)) {
    return { error: "scripted background process list result does not match", processId: null };
  }
  return { error: null, processId: match.session_id };
}

function completeScriptedBackgroundTerminalResponse(response, body) {
  if (!scriptedBackgroundTerminal) return false;
  const messages = body.messages;
  const lastUserMessage = messages.findLast((message) => message?.role === "user");
  const launchRun = lastUserMessage?.content === scriptedBackgroundTerminal.prompt;
  const processRun = lastUserMessage?.content === scriptedBackgroundTerminal.processPrompt;
  if (!launchRun && !processRun) return false;

  const requiredTool = launchRun ? "terminal" : "process";
  const definitions = Array.isArray(body.tools) ? body.tools : [];
  const toolAvailable = definitions.some((definition) => (
    definition?.type === "function" && definition?.function?.name === requiredTool
  ));
  if (!toolAvailable) {
    return rejectScriptedBackgroundTerminal(
      response,
      `scripted background ${requiredTool} tool is unavailable`,
    );
  }

  const terminalProcess = backgroundTerminalProcessId(messages);
  if (terminalProcess.error !== null) {
    return rejectScriptedBackgroundTerminal(response, terminalProcess.error);
  }
  const lastMessage = messages.at(-1);
  const lastCallId = lastMessage?.role === "tool" ? lastMessage.tool_call_id : null;
  let toolCall = null;
  let finalReply = null;

  if (launchRun) {
    if (lastMessage === lastUserMessage) {
      toolCall = {
        arguments: {
          background: true,
          command: scriptedBackgroundTerminal.command,
          notify_on_complete: true,
        },
        id: scriptedBackgroundTerminal.terminalCallId,
        name: "terminal",
      };
    } else if (lastCallId === scriptedBackgroundTerminal.terminalCallId
      && terminalProcess.processId !== null) {
      finalReply = scriptedBackgroundTerminal.reply;
    } else {
      return rejectScriptedBackgroundTerminal(
        response,
        "scripted background terminal launch continuation is invalid",
      );
    }
  } else if (lastMessage === lastUserMessage) {
    toolCall = {
      arguments: { action: "list" },
      id: scriptedBackgroundTerminal.listCallId,
      name: "process",
    };
  } else if (lastCallId === scriptedBackgroundTerminal.listCallId) {
    const listedProcess = backgroundListedProcess(messages, terminalProcess.processId);
    if (listedProcess.error !== null) {
      return rejectScriptedBackgroundTerminal(response, listedProcess.error);
    }
    toolCall = {
      arguments: { action: "kill", session_id: listedProcess.processId },
      id: scriptedBackgroundTerminal.killCallId,
      name: "process",
    };
  } else if (lastCallId === scriptedBackgroundTerminal.killCallId) {
    const listedProcess = backgroundListedProcess(messages, terminalProcess.processId);
    if (listedProcess.error !== null) {
      return rejectScriptedBackgroundTerminal(response, listedProcess.error);
    }
    let killResult;
    try {
      killResult = JSON.parse(lastMessage.content);
    } catch {
      return rejectScriptedBackgroundTerminal(
        response,
        "scripted background process kill result is invalid",
      );
    }
    if (killResult?.session_id !== listedProcess.processId
      || killResult.status !== "killed"
      || typeof killResult.output !== "string"
      || !killResult.output.includes(scriptedBackgroundTerminal.privateOutput)) {
      return rejectScriptedBackgroundTerminal(
        response,
        "scripted background process kill result does not match",
      );
    }
    finalReply = scriptedBackgroundTerminal.processReply;
  } else {
    return rejectScriptedBackgroundTerminal(
      response,
      "scripted background process continuation is invalid",
    );
  }

  response.writeHead(200, {
    "Cache-Control": "no-cache",
    Connection: "keep-alive",
    "Content-Type": "text/event-stream; charset=utf-8",
  });
  if (toolCall) {
    writeEvent(response, {
      choices: [{
        delta: {
          tool_calls: [{
            function: {
              arguments: JSON.stringify(toolCall.arguments),
              name: toolCall.name,
            },
            id: toolCall.id,
            index: 0,
            type: "function",
          }],
        },
        finish_reason: "tool_calls",
        index: 0,
      }],
      usage: {
        completion_tokens: completionTokens,
        prompt_tokens: promptTokens,
        total_tokens: totalTokens,
      },
    });
  } else {
    writeEvent(response, {
      choices: [{
        delta: { content: finalReply },
        finish_reason: "stop",
        index: 0,
      }],
      usage: {
        completion_tokens: completionTokens,
        prompt_tokens: promptTokens,
        total_tokens: totalTokens,
      },
    });
  }
  response.end("data: [DONE]\n\n");
  return true;
}

function completeScriptedCodeResponse(response, body) {
  if (!scriptedCode) return false;
  const messages = body.messages;
  const scriptedConversation = messages.some((message) => (
    message?.role === "user" && message?.content === scriptedCode.prompt
  ));
  if (!scriptedConversation) return false;

  const lastMessage = messages.at(-1);
  const continuingToolCall = lastMessage?.role === "tool"
    && lastMessage?.tool_call_id === scriptedCode.callId;
  if (continuingToolCall) {
    if (typeof lastMessage.content !== "string"
      || !lastMessage.content.includes(scriptedCode.privateOutput)
      || !lastMessage.content.includes('"status":"success"')) {
      response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("scripted code result does not match");
      return true;
    }
  } else {
    const definitions = Array.isArray(body.tools) ? body.tools : [];
    const codeAvailable = definitions.some((definition) => (
      definition?.type === "function"
      && definition?.function?.name === "execute_code"
    ));
    if (!codeAvailable) {
      response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("scripted execute_code tool is unavailable");
      return true;
    }
  }

  response.writeHead(200, {
    "Cache-Control": "no-cache",
    Connection: "keep-alive",
    "Content-Type": "text/event-stream; charset=utf-8",
  });
  if (continuingToolCall) {
    writeEvent(response, {
      choices: [{
        delta: { content: scriptedCode.reply },
        finish_reason: "stop",
        index: 0,
      }],
      usage: {
        completion_tokens: completionTokens,
        prompt_tokens: promptTokens,
        total_tokens: totalTokens,
      },
    });
  } else {
    writeEvent(response, {
      choices: [{
        delta: {
          tool_calls: [{
            function: {
              arguments: JSON.stringify({ code: scriptedCode.source }),
              name: "execute_code",
            },
            id: scriptedCode.callId,
            index: 0,
            type: "function",
          }],
        },
        finish_reason: "tool_calls",
        index: 0,
      }],
      usage: {
        completion_tokens: completionTokens,
        prompt_tokens: promptTokens,
        total_tokens: totalTokens,
      },
    });
  }
  response.end("data: [DONE]\n\n");
  return true;
}

function completeScriptedBrowserResponse(response, body) {
  if (!scriptedBrowser) return false;
  const messages = body.messages;
  const scriptedConversation = messages.some((message) => (
    message?.role === "user" && message?.content === scriptedBrowser.prompt
  ));
  if (!scriptedConversation) return false;

  const definitions = Array.isArray(body.tools) ? body.tools : [];
  const requiredTools = [
    "browser_navigate",
    "browser_snapshot",
    "browser_cdp",
    "browser_download",
  ];
  if (!requiredTools.every((name) => definitions.some((definition) => (
    definition?.type === "function" && definition?.function?.name === name
  )))) {
    response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
    response.end("scripted Browser tools are unavailable");
    return true;
  }

  const lastMessage = messages.at(-1);
  const lastCallId = lastMessage?.role === "tool" ? lastMessage.tool_call_id : null;
  let toolCall = null;
  if (lastCallId === null) {
    toolCall = {
      arguments: { url: scriptedBrowser.url },
      id: scriptedBrowser.navigateCallId,
      name: "browser_navigate",
    };
  } else if (lastCallId === scriptedBrowser.navigateCallId) {
    if (typeof lastMessage.content !== "string" || !lastMessage.content.includes('"navigated":true')) {
      response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("scripted Browser navigation result does not match");
      return true;
    }
    toolCall = {
      arguments: {},
      id: scriptedBrowser.snapshotCallId,
      name: "browser_snapshot",
    };
  } else if (lastCallId === scriptedBrowser.snapshotCallId) {
    let snapshot;
    try {
      snapshot = JSON.parse(lastMessage.content);
    } catch {
      response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("scripted Browser snapshot is invalid");
      return true;
    }
    if (typeof snapshot?.snapshotId !== "string"
      || !snapshot.snapshotId.startsWith("snapshot_")
      || snapshot.title !== scriptedBrowser.expectedTitle) {
      response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("scripted Browser snapshot does not match");
      return true;
    }
    toolCall = {
      arguments: {
        method: "Runtime.evaluate",
        expression: `(() => {
          const id = ${JSON.stringify(scriptedBrowser.downloadSelector.slice(1))};
          document.getElementById(id)?.remove();
          const link = document.createElement("a");
          link.id = id;
          link.download = ${JSON.stringify(scriptedBrowser.downloadFilename)};
          link.href = ${JSON.stringify(
            `data:text/plain;base64,${Buffer.from(
              scriptedBrowser.downloadPrivateContent,
              "utf8",
            ).toString("base64")}`,
          )};
          link.textContent = "Deterministic Browser download fixture";
          document.body.append(link);
          return "synthchat-e2e-download-ready";
        })()`,
        snapshotId: snapshot.snapshotId,
      },
      id: scriptedBrowser.cdpCallId,
      name: "browser_cdp",
    };
  } else if (lastCallId === scriptedBrowser.cdpCallId) {
    if (typeof lastMessage.content !== "string"
      || !lastMessage.content.includes("synthchat-e2e-download-ready")) {
      response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("scripted Browser CDP result does not match");
      return true;
    }
    toolCall = {
      arguments: {},
      id: scriptedBrowser.postCdpSnapshotCallId,
      name: "browser_snapshot",
    };
  } else if (lastCallId === scriptedBrowser.postCdpSnapshotCallId) {
    let snapshot;
    let initialSnapshot;
    try {
      snapshot = JSON.parse(lastMessage.content);
      initialSnapshot = JSON.parse(messages.find((message) => (
        message?.role === "tool"
        && message?.tool_call_id === scriptedBrowser.snapshotCallId
      ))?.content);
    } catch {
      response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("scripted Browser post-CDP snapshot is invalid");
      return true;
    }
    if (typeof snapshot?.snapshotId !== "string"
      || !snapshot.snapshotId.startsWith("snapshot_")
      || snapshot.snapshotId === initialSnapshot?.snapshotId
      || snapshot.title !== scriptedBrowser.expectedTitle
      || !JSON.stringify(snapshot).includes("Deterministic Browser download fixture")) {
      response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("scripted Browser post-CDP snapshot does not match");
      return true;
    }
    toolCall = {
      arguments: {
        selector: scriptedBrowser.downloadSelector,
        snapshotId: snapshot.snapshotId,
      },
      id: scriptedBrowser.downloadCallId,
      name: "browser_download",
    };
  } else if (lastCallId === scriptedBrowser.downloadCallId) {
    let projection;
    try {
      projection = JSON.parse(lastMessage.content)?.download;
    } catch {
      response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("scripted Browser download result is invalid");
      return true;
    }
    const expectedChecks = ["isolated_path", "filename", "mime", "size", "sha256"];
    const projectionText = JSON.stringify(projection);
    if (projection?.name !== scriptedBrowser.downloadFilename
      || projection?.mimeType !== "text/plain"
      || projection?.sizeBytes !== scriptedBrowser.downloadSizeBytes
      || projection?.sha256 !== scriptedBrowser.downloadSha256
      || projection?.scan?.status !== "accepted"
      || JSON.stringify(projection?.scan?.checks) !== JSON.stringify(expectedChecks)
      || projection?.scan?.contentExposed !== false
      || projection?.scan?.workspaceImported !== false
      || projectionText.includes(scriptedBrowser.downloadPrivateContent)
      || /"(?:filePath|downloadPath|path)"\s*:/iu.test(projectionText)) {
      response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("scripted Browser download projection does not match");
      return true;
    }
  } else {
    response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
    response.end("scripted Browser continuation is invalid");
    return true;
  }

  response.writeHead(200, {
    "Cache-Control": "no-cache",
    Connection: "keep-alive",
    "Content-Type": "text/event-stream; charset=utf-8",
  });
  if (toolCall) {
    writeEvent(response, {
      choices: [{
        delta: {
          tool_calls: [{
            function: {
              arguments: JSON.stringify(toolCall.arguments),
              name: toolCall.name,
            },
            id: toolCall.id,
            index: 0,
            type: "function",
          }],
        },
        finish_reason: "tool_calls",
        index: 0,
      }],
      usage: {
        completion_tokens: completionTokens,
        prompt_tokens: promptTokens,
        total_tokens: totalTokens,
      },
    });
  } else {
    writeEvent(response, {
      choices: [{
        delta: { content: scriptedBrowser.reply },
        finish_reason: "stop",
        index: 0,
      }],
      usage: {
        completion_tokens: completionTokens,
        prompt_tokens: promptTokens,
        total_tokens: totalTokens,
      },
    });
  }
  response.end("data: [DONE]\n\n");
  return true;
}

const server = createServer((request, response) => {
  const requestURL = new URL(request.url || "/", origin(host, server.address().port));
  if (request.method !== "POST" || requestURL.pathname !== routePath) {
    response.writeHead(404, { "Content-Type": "text/plain; charset=utf-8" });
    response.end("not found");
    return;
  }

  const chunks = [];
  let size = 0;
  request.on("data", (chunk) => {
    size += chunk.length;
    if (size > maxRequestBytes) {
      response.writeHead(413, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("request too large");
      request.destroy();
      return;
    }
    chunks.push(chunk);
  });
  request.on("end", () => {
    if (response.writableEnded) return;
    let body;
    try {
      body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    } catch {
      response.writeHead(400, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("invalid json");
      return;
    }
    if (
      body?.stream !== true
      || body?.stream_options?.include_usage !== true
      || typeof body?.model !== "string"
      || !Array.isArray(body?.messages)
      || body.messages.length === 0
    ) {
      response.writeHead(422, { "Content-Type": "text/plain; charset=utf-8" });
      response.end("invalid OpenAI-compatible request");
      return;
    }

    requestCount += 1;
    if (completeScriptedApprovalResponse(response, body)) return;
    if (completeScriptedClarificationResponse(response, body)) return;
    if (completeScriptedMcpResponse(response, body)) return;
    if (completeScriptedTerminalResponse(response, body)) return;
    if (completeScriptedBackgroundTerminalResponse(response, body)) return;
    if (completeScriptedCodeResponse(response, body)) return;
    if (completeScriptedBrowserResponse(response, body)) return;
    response.writeHead(200, {
      "Cache-Control": "no-cache",
      Connection: "keep-alive",
      "Content-Type": "text/event-stream; charset=utf-8",
    });
    writeEvent(response, {
      choices: [{ delta: { content: reply.slice(0, midpoint) }, finish_reason: null, index: 0 }],
    });

    if (controlState === "armed") {
      controlState = "holding";
      heldResponse = response;
      response.once("close", () => {
        if (heldResponse !== response) return;
        heldResponse = null;
        controlState = "idle";
      });
      return;
    }
    setImmediate(() => completeResponse(response));
  });
});

const controlServer = createServer((request, response) => {
  const url = requestPath(request);
  if (
    !url
    || request.headers.origin !== undefined
    || !secureEqual(request.headers[controlHeader], controlCapability)
  ) {
    jsonResponse(response, 403, { error: "forbidden" });
    return;
  }
  if (request.headers["transfer-encoding"] !== undefined) {
    jsonResponse(response, 400, { error: "request_body_not_allowed" });
    return;
  }
  const contentLength = Number(request.headers["content-length"] || "0");
  if (!Number.isSafeInteger(contentLength) || contentLength !== 0) {
    jsonResponse(response, 400, { error: "request_body_not_allowed" });
    return;
  }

  if (request.method === "GET" && url.pathname === "/status") {
    jsonResponse(response, 200, { requestCount, state: controlState });
    return;
  }
  if (request.method === "POST" && url.pathname === "/arm") {
    if (controlState !== "idle") {
      jsonResponse(response, 409, { error: "provider_not_idle" });
      return;
    }
    controlState = "armed";
    jsonResponse(response, 200, { requestCount, state: controlState });
    return;
  }
  if (request.method === "POST" && url.pathname === "/release") {
    if (controlState !== "holding" || !heldResponse) {
      jsonResponse(response, 409, { error: "provider_not_holding" });
      return;
    }
    const released = heldResponse;
    heldResponse = null;
    controlState = "idle";
    completeResponse(released);
    jsonResponse(response, 200, { requestCount, state: controlState });
    return;
  }
  jsonResponse(response, 404, { error: "not_found" });
});

const providerOrigin = await listen(server, host, port, "Mock provider");
const controlOrigin = await listen(controlServer, host, controlPort, "Mock provider control");
process.stdout.write(`${JSON.stringify({
  baseUrl: `${providerOrigin}${routePath.replace(/\/chat\/completions$/u, "")}`,
  controlUrl: controlOrigin,
  event: "ready",
})}\n`);

let closing = false;
async function shutdown() {
  if (closing) return;
  closing = true;
  heldResponse?.destroy();
  server.closeAllConnections?.();
  controlServer.closeAllConnections?.();
  await Promise.all([
    new Promise((resolveClosed) => server.close(resolveClosed)),
    new Promise((resolveClosed) => controlServer.close(resolveClosed)),
  ]);
  process.exit(0);
}

process.once("SIGINT", () => void shutdown());
process.once("SIGTERM", () => void shutdown());
if (!process.stdin.isTTY) {
  process.stdin.resume();
  process.stdin.once("end", () => void shutdown());
}
