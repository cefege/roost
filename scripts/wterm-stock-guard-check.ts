// Expiry check for ONE delta: the mouse-report guard in the roost wterm patch.
//
// scripts/rebuild-wterm-wasm.sh runs this against the STOCK upstream build it
// just proved byte-identical to upstream's committed artifact. Roost guards the
// `<`/`=` private markers because upstream routes them into the main CSI switch,
// so an SGR-1006 press report executes deleteLines. It reads the module's raw
// exports so the script keeps its short prerequisite list.
//
// Exit 0: still broken upstream, guard still required. Exit 1: upstream fixed
// it, drop the hunk. Exit 2: the check itself could not run.

const CELL_BYTES = 12;
const GRID_COLS = 40;
const GRID_ROWS = 6;

interface StockCoreExports {
  memory: WebAssembly.Memory;
  init(cols: number, rows: number): void;
  getWriteBuffer(): number;
  writeBytes(length: number): void;
  getGridPtr(): number;
  getMaxCols(): number;
}

function fail(message: string): never {
  console.error(`wterm-stock-guard-check: ${message}`);
  process.exit(2);
}

const wasmPath = Bun.argv[2] ?? fail("expected a wasm path argument");
const core = await (async (): Promise<StockCoreExports> => {
  try {
    const module = await WebAssembly.compile(await Bun.file(wasmPath).arrayBuffer());
    return (await WebAssembly.instantiate(module, {})).exports as unknown as StockCoreExports;
  } catch (error) {
    fail(`could not instantiate ${wasmPath}: ${error instanceof Error ? error.message : String(error)}`);
  }
})();

function write(text: string): void {
  const bytes = new TextEncoder().encode(text);
  new Uint8Array(core.memory.buffer, core.getWriteBuffer(), bytes.length).set(bytes);
  core.writeBytes(bytes.length);
}

function rowText(row: number): string {
  const stride = core.getMaxCols() * CELL_BYTES;
  const cells = new DataView(core.memory.buffer, core.getGridPtr() + (row * stride), stride);
  let text = "";
  for (let col = 0; col < GRID_COLS; col++) {
    const codePoint = cells.getUint32(col * CELL_BYTES, true);
    text += codePoint === 0 ? " " : String.fromCodePoint(codePoint);
  }
  return text.trimEnd();
}

core.init(GRID_COLS, GRID_ROWS);
for (const [index, label] of ["ROW0", "ROW1", "ROW2", "ROW3"].entries()) {
  write(`\x1b[${index + 1};1H${label}`);
}
if (rowText(1) !== "ROW1") fail(`fixture did not paint: row 1 is ${JSON.stringify(rowText(1))}`);

// deleteLines acts at the cursor, so the report must arrive with the cursor
// parked on the row whose survival is the observable.
write("\x1b[2;1H\x1b[<0;10;5M");
if (rowText(1) === "ROW1") {
  console.log(
    "Good news, and an instruction: stock upstream no longer executes `ESC[<0;10;5M`\n"
    + "as deleteLines, so the mouse-report hunk in the patch is redundant. Drop that hunk\n"
    + "(the `<`/`=` guard in src/terminal.zig handleCsi), re-run this script, and delete the\n"
    + "press/release cases in apps/shared/tests/wterm-mouse-report-inert.test.ts.\n"
    + "Nothing is broken — the build is fine and this is the only thing to change.",
  );
  process.exit(1);
}
console.log("==> stock still runs SGR-1006 press reports as deleteLines; mouse-report hunk required");
