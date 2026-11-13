import { Terminal } from "@xterm/headless";
import { appendFileSync } from "node:fs";
import { mkdir } from "node:fs/promises";
import { dirname } from "node:path";
import { within, type RunningChild } from "./runtime";

export interface TerminalSnapshot {
  text: string;
  lines: string[];
  scrollback: string[];
  cellColumns?: number[][];
  cursor: { column: number; row: number };
  columns: number;
  rows: number;
}

export interface TerminalCellColors {
  foreground?: string;
  background?: string;
}

export interface TerminalCellStyle {
  bold: boolean;
  italic: boolean;
  underline: boolean;
  inverse: boolean;
}

export interface TerminalLeg extends RunningChild {
  input(bytes: string): void;
  resize(columns: number, rows: number): void;
  snapshot(includeCellPositions?: boolean): Promise<TerminalSnapshot>;
  cellColors(row: number, column: number): TerminalCellColors;
  cellStyle(row: number, column: number): TerminalCellStyle;
  readOutput(): Promise<string>;
}

export async function startTerminal(
  command: string[],
  cwd: string,
  env: Record<string, string>,
  rawCapture: string,
  columns = 120,
  rows = 48,
  options: { hostTracePath?: string } = {},
): Promise<TerminalLeg> {
  await mkdir(dirname(rawCapture), { recursive: true });
  const capture = Bun.file(rawCapture).writer();
  const screen = new Terminal({ cols: columns, rows, scrollback: 20_000, allowProposedApi: true });
  let painted = Promise.resolve();
  let generation = 0;
  const terminal = new Bun.Terminal({
    cols: columns,
    rows,
    data(_terminal, bytes) {
      capture.write(bytes);
      generation++;
      const completion = Promise.withResolvers<void>();
      screen.write(bytes, completion.resolve);
      painted = completion.promise;
    },
  });
  const responses = screen.onData(bytes => terminal.write(bytes));
  let clipboard = "";
  const clipboardHandler = screen.parser.registerOscHandler(52, data => {
    const separator = data.indexOf(";");
    if (separator < 0) return false;
    const payload = data.slice(separator + 1);
    if (payload === "?") {
      terminal.write(`\x1b]52;c;${Buffer.from(clipboard).toString("base64")}\x1b\\`);
      return true;
    }
    if (!/^[A-Za-z0-9+/]*={0,2}$/.test(payload)) throw new Error("Actual TUI emitted malformed OSC52 clipboard data");
    clipboard = Buffer.from(payload, "base64").toString("utf8");
    if (options.hostTracePath) appendFileSync(options.hostTracePath, `${JSON.stringify({ channel: "builtin.clipboard", value: clipboard })}\n`);
    return true;
  });
  const child = Bun.spawn(command, { cwd, env, terminal, detached: true });
  let stopped = false;
  return {
    pid: child.pid,
    exited: child.exited,
    get exitCode() { return child.exitCode; },
    input(bytes) {
      if (child.exitCode !== null) throw new Error(`TUI exited ${child.exitCode} before input`);
      terminal.write(bytes);
    },
    resize(nextColumns, nextRows) {
      screen.resize(nextColumns, nextRows);
      terminal.resize(nextColumns, nextRows);
    },
    async snapshot(includeCellPositions = false) {
      let observed: number;
      do {
        observed = generation;
        await painted;
      } while (observed !== generation);
      const buffer = screen.buffer.active;
      const lines = Array.from({ length: screen.rows }, (_, offset) =>
        buffer.getLine(buffer.viewportY + offset)?.translateToString(true) ?? "",
      );
      const scrollback = Array.from({ length: buffer.length }, (_, offset) =>
        buffer.getLine(offset)?.translateToString(true) ?? "",
      );
      const cellColumns = includeCellPositions ? Array.from({ length: screen.rows }, (_, row) => {
        const line = buffer.getLine(buffer.viewportY + row);
        const offsets: number[] = [];
        for (let column = 0; column < screen.cols; column++) {
          const cell = line?.getCell(column);
          if (cell?.getWidth() === 0) continue;
          const characters = cell?.getChars() || " ";
          for (let offset = 0; offset < characters.length; offset++) offsets.push(column);
        }
        offsets.push(screen.cols);
        return offsets;
      }) : undefined;
      return {
        text: lines.join("\n"),
        lines,
        scrollback,
        ...(cellColumns ? { cellColumns } : {}),
        cursor: { column: buffer.cursorX, row: buffer.cursorY },
        columns: screen.cols,
        rows: screen.rows,
      };
    },
    cellColors(row, column) {
      const cell = screen.buffer.active.getLine(screen.buffer.active.viewportY + row)?.getCell(column);
      if (!cell) throw new Error(`Actual terminal cell ${column},${row} is unavailable`);
      const foreground = cell.getFgColor();
      const background = cell.getBgColor();
      return {
        ...(cell.isFgRGB() ? { foreground: `rgb(${(foreground >>> 16) & 255}, ${(foreground >>> 8) & 255}, ${foreground & 255})` } : {}),
        ...(cell.isBgRGB() ? { background: `rgb(${(background >>> 16) & 255}, ${(background >>> 8) & 255}, ${background & 255})` } : {}),
      };
    },
    cellStyle(row, column) {
      const cell = screen.buffer.active.getLine(screen.buffer.active.viewportY + row)?.getCell(column);
      if (!cell) throw new Error(`Actual terminal cell ${column},${row} is unavailable`);
      return {
        bold: Boolean(cell.isBold()),
        italic: Boolean(cell.isItalic()),
        underline: Boolean(cell.isUnderline()),
        inverse: Boolean(cell.isInverse()),
      };
    },
    async readOutput() {
      if (!stopped) await capture.flush();
      return Bun.file(rawCapture).text();
    },
    async stop() {
      if (stopped) return;
      stopped = true;
      try {
        if (child.exitCode === null) {
          try { process.kill(-child.pid, "SIGTERM"); } catch { child.kill("SIGTERM"); }
          try {
            await within(child.exited, "TUI shutdown", 5_000);
          } catch {
            try { process.kill(-child.pid, "SIGKILL"); } catch { child.kill("SIGKILL"); }
            await child.exited;
          }
        }
        await painted;
        await capture.flush();
      } finally {
        terminal.close();
        responses.dispose();
        clipboardHandler.dispose();
        screen.dispose();
        capture.end();
      }
    },
  };
}
