import { readdir, readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import ts from "../frontend/node_modules/typescript/lib/typescript.js";

const CORE_MODULE = "@tauri-apps/api/core";
const CONNECTION_BRIDGE = "frontend/src/api/desktopConnection.ts";
const CONFIG_BRIDGE = "frontend/src/config/runtimeConfig/desktopBridge.ts";
const PET_BRIDGE = "frontend/src/features/pet/desktopPet.ts";
const INVOKE_BRIDGES = new Set([
  CONNECTION_BRIDGE,
  CONFIG_BRIDGE,
  PET_BRIDGE,
]);
const PET_COMMANDS = new Map([
  ["open", "open_pet_window"],
  ["toggle", "toggle_pet_window"],
  ["startDragging", "pet_window_start_dragging"],
  ["setIgnoreCursorEvents", "pet_window_set_ignore_cursor_events"],
]);

const repositoryRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
);
const frontendSourceRoot = path.join(repositoryRoot, "frontend", "src");

function relativePath(filePath) {
  return path.relative(repositoryRoot, filePath).split(path.sep).join("/");
}

function isProductionTypeScript(filePath) {
  const normalized = relativePath(filePath);
  return /\.[cm]?tsx?$/u.test(normalized)
    && !/\.(?:test|spec|mock)\.[cm]?tsx?$/u.test(normalized)
    && !/\.d\.[cm]?ts$/u.test(normalized)
    && !normalized.split("/").includes("__mocks__");
}

async function collectProductionSources(directory = frontendSourceRoot) {
  const entries = await readdir(directory, { withFileTypes: true });
  const sources = new Map();
  for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
    const filePath = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      for (const [nestedPath, source] of await collectProductionSources(filePath)) {
        sources.set(nestedPath, source);
      }
    } else if (entry.isFile() && isProductionTypeScript(filePath)) {
      sources.set(relativePath(filePath), await readFile(filePath, "utf8"));
    }
  }
  return sources;
}

function scriptKind(filePath) {
  return filePath.endsWith(".tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS;
}

function parseSource(filePath, sourceText) {
  const sourceFile = ts.createSourceFile(
    filePath,
    sourceText,
    ts.ScriptTarget.Latest,
    true,
    scriptKind(filePath),
  );
  if (sourceFile.parseDiagnostics.length > 0) {
    const first = sourceFile.parseDiagnostics[0];
    throw new Error(`${filePath}: TypeScript parse error ${first.code}.`);
  }
  return sourceFile;
}

function unwrapExpression(expression) {
  let current = expression;
  while (
    ts.isParenthesizedExpression(current)
    || ts.isAsExpression(current)
    || ts.isTypeAssertionExpression(current)
    || ts.isNonNullExpression(current)
    || ts.isSatisfiesExpression(current)
  ) {
    current = current.expression;
  }
  return current;
}

function visit(node, callback) {
  callback(node);
  ts.forEachChild(node, (child) => visit(child, callback));
}

function moduleName(importDeclaration) {
  return ts.isStringLiteral(importDeclaration.moduleSpecifier)
    ? importDeclaration.moduleSpecifier.text
    : undefined;
}

function importedInvokeBinding(sourceFile, filePath, violations) {
  const bindings = [];
  for (const statement of sourceFile.statements) {
    if (ts.isExportDeclaration(statement) && moduleName(statement) === CORE_MODULE) {
      violations.push(`${filePath}: re-exporting ${CORE_MODULE} is forbidden.`);
      continue;
    }
    if (!ts.isImportDeclaration(statement) || moduleName(statement) !== CORE_MODULE) continue;
    if (statement.importClause?.name) {
      violations.push(`${filePath}: default imports from ${CORE_MODULE} are forbidden.`);
    }
    const namedBindings = statement.importClause?.namedBindings;
    if (namedBindings && ts.isNamespaceImport(namedBindings)) {
      violations.push(`${filePath}: namespace imports from ${CORE_MODULE} are forbidden.`);
      continue;
    }
    if (!namedBindings || !ts.isNamedImports(namedBindings)) continue;
    for (const specifier of namedBindings.elements) {
      const importedName = specifier.propertyName?.text ?? specifier.name.text;
      if (importedName !== "invoke") continue;
      if (specifier.name.text !== "invoke") {
        violations.push(`${filePath}: the Tauri invoke import must not be aliased.`);
      }
      bindings.push({ name: specifier.name.text, specifier });
    }
  }
  if (bindings.length > 1) {
    violations.push(`${filePath}: Tauri invoke must be imported at most once.`);
  }
  return bindings[0];
}

function isPropertyNameIdentifier(node) {
  const parent = node.parent;
  return (
    (ts.isPropertyAccessExpression(parent) && parent.name === node)
    || (ts.isPropertyAssignment(parent) && parent.name === node && !ts.isShorthandPropertyAssignment(parent))
    || (ts.isPropertySignature(parent) && parent.name === node)
    || (ts.isMethodSignature(parent) && parent.name === node)
    || (ts.isMethodDeclaration(parent) && parent.name === node)
    || (ts.isPropertyDeclaration(parent) && parent.name === node)
  );
}

function bindingReferences(sourceFile, binding) {
  if (!binding) return [];
  const references = [];
  visit(sourceFile, (node) => {
    if (
      ts.isIdentifier(node)
      && node.text === binding.name
      && node !== binding.specifier.name
      && !isPropertyNameIdentifier(node)
    ) {
      references.push(node);
    }
  });
  return references;
}

function directBindingCalls(sourceFile, binding) {
  if (!binding) return [];
  const calls = [];
  visit(sourceFile, (node) => {
    if (!ts.isCallExpression(node)) return;
    const callee = unwrapExpression(node.expression);
    if (ts.isIdentifier(callee) && callee.text === binding.name) calls.push(node);
  });
  return calls;
}

function namedCalls(sourceFile, name) {
  const calls = [];
  visit(sourceFile, (node) => {
    if (!ts.isCallExpression(node)) return;
    const callee = unwrapExpression(node.expression);
    if (ts.isIdentifier(callee) && callee.text === name) calls.push(node);
  });
  return calls;
}

function namedValueReferences(sourceFile, name) {
  const references = [];
  visit(sourceFile, (node) => {
    if (!ts.isIdentifier(node) || node.text !== name || isPropertyNameIdentifier(node)) return;
    if (ts.isVariableDeclaration(node.parent) && node.parent.name === node) return;
    references.push(node);
  });
  return references;
}

function forbiddenInvokePropertyCalls(sourceFile) {
  const calls = [];
  visit(sourceFile, (node) => {
    if (!ts.isCallExpression(node)) return;
    const callee = unwrapExpression(node.expression);
    if (ts.isPropertyAccessExpression(callee) && callee.name.text === "invoke") {
      calls.push(node);
    }
  });
  return calls;
}

function dynamicCoreLoads(sourceFile) {
  const calls = [];
  visit(sourceFile, (node) => {
    if (!ts.isCallExpression(node) || node.arguments.length === 0) return;
    const firstArgument = node.arguments[0];
    if (!ts.isStringLiteral(firstArgument) || firstArgument.text !== CORE_MODULE) return;
    const callee = unwrapExpression(node.expression);
    if (
      callee.kind === ts.SyntaxKind.ImportKeyword
      || (ts.isIdentifier(callee) && callee.text === "require")
    ) {
      calls.push(node);
    }
  });
  return calls;
}

function constInitializer(sourceFile, name) {
  let initializer;
  visit(sourceFile, (node) => {
    if (
      ts.isVariableDeclaration(node)
      && ts.isIdentifier(node.name)
      && node.name.text === name
    ) {
      if (initializer !== undefined) throw new Error(`${name} is declared more than once.`);
      const declarationList = node.parent;
      if (!ts.isVariableDeclarationList(declarationList) || !(declarationList.flags & ts.NodeFlags.Const)) {
        throw new Error(`${name} must be a const declaration.`);
      }
      initializer = node.initializer;
    }
  });
  if (!initializer) throw new Error(`${name} const declaration is missing.`);
  return initializer;
}

function expectStringConst(sourceFile, filePath, name, value, violations) {
  try {
    const initializer = constInitializer(sourceFile, name);
    if (!ts.isStringLiteral(initializer) || initializer.text !== value) {
      violations.push(`${filePath}: ${name} must equal ${JSON.stringify(value)}.`);
    }
  } catch (error) {
    violations.push(`${filePath}: ${error.message}`);
  }
}

function expectSingleCommandCall(
  sourceFile,
  filePath,
  binding,
  constantName,
  command,
  violations,
) {
  expectStringConst(sourceFile, filePath, constantName, command, violations);
  const calls = directBindingCalls(sourceFile, binding);
  if (calls.length !== 1) {
    violations.push(`${filePath}: expected exactly one direct Tauri invoke call, found ${calls.length}.`);
    return;
  }
  const call = calls[0];
  if (
    call.arguments.length !== 1
    || !ts.isIdentifier(unwrapExpression(call.arguments[0]))
    || unwrapExpression(call.arguments[0]).text !== constantName
  ) {
    violations.push(`${filePath}: Tauri invoke must receive only ${constantName}.`);
  }
  const references = bindingReferences(sourceFile, binding);
  if (references.length !== 1 || unwrapExpression(call.expression) !== references[0]) {
    violations.push(`${filePath}: the imported Tauri invoke binding escaped its fixed call.`);
  }
}

function expectNoParameterLoader(sourceFile, filePath, violations) {
  const matches = sourceFile.statements.filter(
    (statement) => ts.isFunctionDeclaration(statement)
      && statement.name?.text === "loadDesktopFrontendRuntimeConfig",
  );
  if (matches.length !== 1 || matches[0].parameters.length !== 0) {
    violations.push(`${filePath}: loadDesktopFrontendRuntimeConfig must be one no-argument function.`);
  }
}

function petCommandObject(sourceFile, filePath, violations) {
  let initializer;
  try {
    initializer = unwrapExpression(constInitializer(sourceFile, "PET_WINDOW_COMMANDS"));
  } catch (error) {
    violations.push(`${filePath}: ${error.message}`);
    return;
  }
  if (!ts.isObjectLiteralExpression(initializer)) {
    violations.push(`${filePath}: PET_WINDOW_COMMANDS must be an object literal.`);
    return;
  }
  const actual = new Map();
  for (const property of initializer.properties) {
    if (
      !ts.isPropertyAssignment(property)
      || !ts.isIdentifier(property.name)
      || !ts.isStringLiteral(property.initializer)
    ) {
      violations.push(`${filePath}: PET_WINDOW_COMMANDS may contain only fixed string properties.`);
      continue;
    }
    actual.set(property.name.text, property.initializer.text);
  }
  if (
    actual.size !== PET_COMMANDS.size
    || [...PET_COMMANDS].some(([key, value]) => actual.get(key) !== value)
  ) {
    violations.push(`${filePath}: PET_WINDOW_COMMANDS changed from its reviewed allowlist.`);
  }
}

function petInvokeInitializer(sourceFile, binding, filePath, violations) {
  let initializer;
  try {
    initializer = unwrapExpression(constInitializer(sourceFile, "invokeCommand"));
  } catch (error) {
    violations.push(`${filePath}: ${error.message}`);
    return;
  }
  if (
    !ts.isBinaryExpression(initializer)
    || initializer.operatorToken.kind !== ts.SyntaxKind.QuestionQuestionToken
    || !ts.isPropertyAccessExpression(initializer.left)
    || !ts.isIdentifier(initializer.left.expression)
    || initializer.left.expression.text !== "dependencies"
    || initializer.left.name.text !== "invoke"
    || !ts.isIdentifier(initializer.right)
    || initializer.right.text !== binding?.name
  ) {
    violations.push(`${filePath}: invokeCommand must be exactly dependencies.invoke ?? invoke.`);
  }
  const references = bindingReferences(sourceFile, binding);
  if (references.length !== 1 || references[0] !== initializer.right) {
    violations.push(`${filePath}: the imported Tauri invoke binding escaped the Pet bridge dependency boundary.`);
  }
}

function expectPetCalls(sourceFile, filePath, binding, violations) {
  petCommandObject(sourceFile, filePath, violations);
  petInvokeInitializer(sourceFile, binding, filePath, violations);
  const calls = namedCalls(sourceFile, "invokeCommand");
  const references = namedValueReferences(sourceFile, "invokeCommand");
  const callCallees = new Set(calls.map((call) => unwrapExpression(call.expression)));
  if (
    references.length !== PET_COMMANDS.size
    || references.some((reference) => !callCallees.has(reference))
  ) {
    violations.push(`${filePath}: invokeCommand escaped its four fixed direct calls.`);
  }
  const counts = new Map([...PET_COMMANDS.keys()].map((key) => [key, 0]));
  for (const call of calls) {
    const command = unwrapExpression(call.arguments[0]);
    if (
      !ts.isPropertyAccessExpression(command)
      || !ts.isIdentifier(command.expression)
      || command.expression.text !== "PET_WINDOW_COMMANDS"
      || !counts.has(command.name.text)
    ) {
      violations.push(`${filePath}: Pet invokeCommand received a non-allowlisted command.`);
      continue;
    }
    const key = command.name.text;
    counts.set(key, counts.get(key) + 1);
    const expectedArguments = key === "setIgnoreCursorEvents" ? 2 : 1;
    if (call.arguments.length !== expectedArguments) {
      violations.push(`${filePath}: PET_WINDOW_COMMANDS.${key} received unexpected arguments.`);
    }
    if (key === "setIgnoreCursorEvents") {
      const options = unwrapExpression(call.arguments[1]);
      if (
        !ts.isObjectLiteralExpression(options)
        || options.properties.length !== 1
        || !ts.isShorthandPropertyAssignment(options.properties[0])
        || options.properties[0].name.text !== "ignore"
      ) {
        violations.push(`${filePath}: setIgnoreCursorEvents may pass only the ignore boolean.`);
      }
    }
  }
  if (calls.length !== PET_COMMANDS.size) {
    violations.push(`${filePath}: expected four fixed Pet invokes, found ${calls.length}.`);
  }
  for (const [key, count] of counts) {
    if (count !== 1) {
      violations.push(`${filePath}: PET_WINDOW_COMMANDS.${key} must be invoked exactly once.`);
    }
  }
}

function verifySources(sources) {
  const violations = [];
  const parsed = new Map();
  for (const [filePath, sourceText] of sources) {
    parsed.set(filePath, parseSource(filePath, sourceText));
  }

  for (const [filePath, sourceFile] of parsed) {
    const binding = importedInvokeBinding(sourceFile, filePath, violations);
    if (binding && !INVOKE_BRIDGES.has(filePath)) {
      violations.push(`${filePath}: Tauri invoke is not allowed outside the three reviewed bridges.`);
    }
    if (dynamicCoreLoads(sourceFile).length > 0) {
      violations.push(`${filePath}: dynamic loading of ${CORE_MODULE} is forbidden.`);
    }
    if (forbiddenInvokePropertyCalls(sourceFile).length > 0) {
      violations.push(`${filePath}: property-based invoke calls are forbidden.`);
    }
  }

  for (const filePath of INVOKE_BRIDGES) {
    if (!parsed.has(filePath)) violations.push(`${filePath}: reviewed bridge is missing.`);
  }
  if (violations.length > 0) return violations;

  const connectionSource = parsed.get(CONNECTION_BRIDGE);
  const connectionBinding = importedInvokeBinding(connectionSource, CONNECTION_BRIDGE, violations);
  expectSingleCommandCall(
    connectionSource,
    CONNECTION_BRIDGE,
    connectionBinding,
    "BACKEND_CONNECTION_COMMAND",
    "get_backend_connection",
    violations,
  );

  const configSource = parsed.get(CONFIG_BRIDGE);
  const configBinding = importedInvokeBinding(configSource, CONFIG_BRIDGE, violations);
  expectSingleCommandCall(
    configSource,
    CONFIG_BRIDGE,
    configBinding,
    "FRONTEND_RUNTIME_CONFIG_COMMAND",
    "get_frontend_runtime_config",
    violations,
  );
  expectNoParameterLoader(configSource, CONFIG_BRIDGE, violations);

  const petSource = parsed.get(PET_BRIDGE);
  const petBinding = importedInvokeBinding(petSource, PET_BRIDGE, violations);
  if (directBindingCalls(petSource, petBinding).length !== 0) {
    violations.push(`${PET_BRIDGE}: raw Tauri invoke calls are forbidden in the Pet bridge.`);
  }
  expectPetCalls(petSource, PET_BRIDGE, petBinding, violations);

  return violations;
}

function withMutation(sources, filePath, suffix) {
  const mutated = new Map(sources);
  mutated.set(filePath, `${sources.get(filePath)}\n${suffix}\n`);
  return mutated;
}

function expectSelfTestFailure(name, sources) {
  const violations = verifySources(sources);
  if (violations.length === 0) {
    throw new Error(`Bridge verifier self-test failed to reject ${name}.`);
  }
}

function runSelfTests(sources) {
  expectSelfTestFailure(
    "a direct raw Pet invoke",
    withMutation(sources, PET_BRIDGE, 'invoke("other_command");'),
  );
  expectSelfTestFailure(
    "multiple invokes on one line",
    withMutation(
      sources,
      CONFIG_BRIDGE,
      "invoke(FRONTEND_RUNTIME_CONFIG_COMMAND); invoke(FRONTEND_RUNTIME_CONFIG_COMMAND);",
    ),
  );
  expectSelfTestFailure(
    "a generic whitespace-form invoke",
    withMutation(sources, PET_BRIDGE, 'invoke <unknown>\n  ("other_command");'),
  );
  expectSelfTestFailure(
    "an indirect Pet invokeCommand call",
    withMutation(
      sources,
      PET_BRIDGE,
      "invokeCommand.call(undefined, PET_WINDOW_COMMANDS.open);",
    ),
  );
}

const argumentsSet = new Set(process.argv.slice(2));
for (const argument of argumentsSet) {
  if (argument !== "--self-test") throw new Error(`Unknown argument: ${argument}`);
}

const sources = await collectProductionSources();
if (argumentsSet.has("--self-test")) runSelfTests(sources);
const violations = verifySources(sources);
if (violations.length > 0) {
  for (const violation of violations) console.error(violation);
  process.exitCode = 1;
} else {
  console.log(
    `Verified ${sources.size} production frontend TypeScript files and three narrow Tauri bridges.`,
  );
}
