import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const workspace = dirname(dirname(fileURLToPath(import.meta.url)));
const frontend = join(workspace, "frontend");
const schema = join(workspace, "docs", "openapi.yaml");
const generated = join(frontend, "src", "api", "generated", "openapi.d.ts");
const cli = join(frontend, "node_modules", "openapi-typescript", "bin", "cli.js");
const temporaryDirectory = await mkdtemp(join(tmpdir(), "synthchat-openapi-"));
const expected = join(temporaryDirectory, "openapi.d.ts");

try {
  const result = spawnSync(process.execPath, [cli, schema, "--output", expected], {
    cwd: frontend,
    stdio: "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);

  let actualBytes;
  try {
    actualBytes = await readFile(generated);
  } catch (error) {
    if (error?.code === "ENOENT") {
      console.error("Generated OpenAPI types are missing. Run npm run api:generate.");
      process.exitCode = 1;
    } else {
      throw error;
    }
  }

  if (actualBytes) {
    const expectedBytes = await readFile(expected);
    if (!actualBytes.equals(expectedBytes)) {
      console.error("Generated OpenAPI types are stale. Run npm run api:generate.");
      process.exitCode = 1;
    }
  }
} finally {
  await rm(temporaryDirectory, { recursive: true, force: true });
}
