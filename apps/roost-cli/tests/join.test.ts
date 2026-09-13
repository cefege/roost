// Enrollment checkout regression coverage.
// `join.sh` preserves a dirty source checkout by cloning or reusing a clean sibling worker.
// It pins that clean worker to the coordinator's detached commit.
import { afterEach, expect, test } from "bun:test";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join, resolve } from "node:path";
import {
  _deployJoinedPosixWorker,
} from "../src/join.ts";
import type { JournaledKeeperUpdateCallbacks } from "../src/direct-keeper-update.ts";

const temporaryRoots: string[] = [];

const ROOT = resolve(import.meta.dir, "../../..");
const JOIN_SCRIPT = join(ROOT, "join.sh");

function executable(path: string, source: string): void {
  writeFileSync(path, source);
  chmodSync(path, 0o755);
}

function git(cwd: string, args: string[]): string {
  const result = Bun.spawnSync(["git", ...args], { cwd });
  if (result.exitCode !== 0) {
    throw new Error(result.stderr.toString());
  }
  return result.stdout.toString().trim();
}

afterEach(() => {
  for (const root of temporaryRoots.splice(0)) rmSync(root, { recursive: true, force: true });
});

test("deploys join.sh's clean detached coordinator commit", async () => {
  const root = mkdtempSync(join(tmpdir(), "roost-join-detached-"));
  temporaryRoots.push(root);
  git(root, ["init", "--quiet"]);
  git(root, ["config", "user.email", "test@example.com"]);
  git(root, ["config", "user.name", "Roost Test"]);
  writeFileSync(join(root, "README"), "join\n");
  git(root, ["add", "README"]);
  git(root, ["commit", "--quiet", "-m", "coordinator commit"]);
  const coordinatorSha = git(root, ["rev-parse", "HEAD"]);
  git(root, ["checkout", "--quiet", "--detach", coordinatorSha]);
  let deployedSha: string | undefined;
  const keeperCallbacks: JournaledKeeperUpdateCallbacks = {
    apply: async () => undefined,
    prove: async () => undefined,
  };

  await _deployJoinedPosixWorker(
    root,
    async (_host, options) => { deployedSha = options.gitSha; },
    keeperCallbacks,
  );

  expect(deployedSha).toBe(coordinatorSha);
});

test.skipIf(process.platform === "win32")(
  "reuses a clean sibling checkout after retrying a dirty source join",
  () => {
    const root = mkdtempSync(join(tmpdir(), "roost-join-dirty-"));
    temporaryRoots.push(root);
    const commands = join(root, "commands");
    const home = join(root, "home");
    const source = join(home, "Roost");
    const sibling = `${source}-worker`;
    const trace = join(root, "commands.log");
    mkdirSync(commands);
    mkdirSync(source, { recursive: true });
    writeFileSync(join(source, ".git"), "gitdir: /linked-worktree\n");
    writeFileSync(join(source, "tracked.txt"), "local tracked change\n");
    writeFileSync(join(source, "untracked.txt"), "local untracked change\n");

    executable(join(commands, "uname"), "#!/bin/sh\nprintf 'Linux\\n'\n");
    executable(join(commands, "git"), `#!/bin/sh
set -eu
{
  printf 'git'
  for argument in "$@"; do printf ' [%s]' "$argument"; done
  printf '\n'
} >> "$TRACE"
if [ "$1" = "clone" ]; then
  [ "$2" = "https://github.com/cefege/roost.git" ] || exit 99
  [ "$3" = "$SIBLING" ] || exit 99
  mkdir -p "$3/.git"
  exit 0
fi
[ "$1" = "-C" ] || exit 99
checkout="$2"
shift 2
case "$1" in
  status)
    [ "$2" = "--porcelain" ] || exit 99
    [ "$#" = 2 ] || exit 99
    if [ "$checkout" = "$SOURCE" ]; then
      printf '%s\n' ' M tracked.txt' '?? untracked.txt'
    elif [ "$checkout" != "$SIBLING" ]; then
      exit 99
    fi
    ;;
  remote)
    [ "$checkout" = "$SIBLING" ] || exit 99
    [ "$2" = "get-url" ] || exit 99
    [ "$3" = "origin" ] || exit 99
    [ "$#" = 3 ] || exit 99
    printf '%s\n' 'https://github.com/cefege/roost.git'
    ;;
  fetch)
    [ "$checkout" = "$SIBLING" ] || exit 99
    [ "$2" = "--quiet" ] || exit 99
    [ "$3" = "origin" ] || exit 99
    [ "$#" = 3 ] || exit 99
    ;;
  cat-file)
    [ "$checkout" = "$SIBLING" ] || exit 99
    [ "$2" = "-e" ] || exit 99
    [ "$3" = "coord-sha^{commit}" ] || exit 99
    ;;
  checkout)
    [ "$checkout" = "$SIBLING" ] || exit 99
    [ "$2" = "--quiet" ] || exit 99
    [ "$3" = "--detach" ] || exit 99
    [ "$4" = "coord-sha" ] || exit 99
    ;;
  *)
    exit 99
    ;;
esac
`);
    executable(join(commands, "curl"), `#!/bin/sh
set -eu
{
  printf 'curl'
  for argument in "$@"; do printf ' [%s]' "$argument"; done
  printf '\n'
} >> "$TRACE"
printf '%s\n' '{"gitSha":"coord-sha"}'
`);
    executable(join(commands, "bun"), `#!/bin/sh
set -eu
{
  printf 'bun [%s]' "$PWD"
  for argument in "$@"; do printf ' [%s]' "$argument"; done
  printf '\n'
} >> "$TRACE"
case "$1" in
  install)
    [ "$#" = 1 ] || exit 99
    ;;
  apps/roost-cli/src/main.ts)
    [ "$2" = "join" ] || exit 99
    ;;
  *)
    exit 99
    ;;
esac
`);

    const environment = { ...process.env };
    delete environment.ROOST_DIR;
    Object.assign(environment, {
      HOME: home,
      PATH: `${commands}${delimiter}${process.env.PATH ?? "/usr/bin:/bin"}`,
      ROOST_COORDINATOR_URL: "https://coord.example.test",
      ROOST_BOOTSTRAP_TOKEN: "test-token",
      SIBLING: sibling,
      SOURCE: source,
      TRACE: trace,
    });
    const firstResult = Bun.spawnSync(["bash", JOIN_SCRIPT], {
      cwd: root,
      env: environment,
    });
    expect(firstResult.exitCode, firstResult.stderr.toString()).toBe(0);
    expect(firstResult.stdout.toString()).toContain(
      `preserving dirty source ${source}; using clean worker checkout ${sibling}`,
    );

    const secondResult = Bun.spawnSync(["bash", JOIN_SCRIPT], {
      cwd: root,
      env: environment,
    });
    expect(secondResult.exitCode, secondResult.stderr.toString()).toBe(0);
    expect(secondResult.stdout.toString()).toContain(
      `preserving dirty source ${source}; reusing clean worker checkout ${sibling}`,
    );

    expect(readFileSync(join(source, "tracked.txt"), "utf8")).toBe("local tracked change\n");
    expect(readFileSync(join(source, "untracked.txt"), "utf8")).toBe("local untracked change\n");
    expect(readFileSync(join(source, ".git"), "utf8")).toBe("gitdir: /linked-worktree\n");
    expect(existsSync(join(sibling, ".git"))).toBe(true);

    const traceLines = readFileSync(trace, "utf8").trim().split("\n");
    const firstRunTrace = [
      `git [-C] [${source}] [status] [--porcelain]`,
      `git [clone] [https://github.com/cefege/roost.git] [${sibling}]`,
      "curl [-fsS] [-m] [8] [-X] [POST] [-H] [Content-Type: application/json] [-d] [{}] [https://coord.example.test/roost.v1.CoordinatorService/MiscHealth]",
      `git [-C] [${sibling}] [cat-file] [-e] [coord-sha^{commit}]`,
      `git [-C] [${sibling}] [checkout] [--quiet] [--detach] [coord-sha]`,
      `bun [${sibling}] [install]`,
      `bun [${sibling}] [apps/roost-cli/src/main.ts] [join]`,
    ];
    expect(traceLines.slice(0, firstRunTrace.length)).toEqual(firstRunTrace);
    expect(traceLines.slice(firstRunTrace.length)).toEqual([
      `git [-C] [${source}] [status] [--porcelain]`,
      `git [-C] [${sibling}] [remote] [get-url] [origin]`,
      `git [-C] [${sibling}] [status] [--porcelain]`,
      `git [-C] [${sibling}] [fetch] [--quiet] [origin]`,
      "curl [-fsS] [-m] [8] [-X] [POST] [-H] [Content-Type: application/json] [-d] [{}] [https://coord.example.test/roost.v1.CoordinatorService/MiscHealth]",
      `git [-C] [${sibling}] [cat-file] [-e] [coord-sha^{commit}]`,
      `git [-C] [${sibling}] [checkout] [--quiet] [--detach] [coord-sha]`,
      `bun [${sibling}] [install]`,
      `bun [${sibling}] [apps/roost-cli/src/main.ts] [join]`,
    ]);
  },
);
