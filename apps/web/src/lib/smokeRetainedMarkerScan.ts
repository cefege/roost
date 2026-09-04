// Rendered DOM rows cannot prove whether older terminal history remains available upstream.
// This scanner pages coordinator-retained cells and checks marker continuity outside the renderer.
// The smoke backdoor uses the reported grid epoch and history floor to distinguish loss causes.
// Strict page validation keeps a moving or malformed snapshot from producing a false pass.

import type { ScrollbackHistoryFloor } from "@roost/shared/wire";
import { coordClient } from "../connect.ts";
import { cellGridEpoch as cellGridEpochImpl } from "../store/sync.ts";
import {
  SCROLLBACK_FLOOR_REASON,
} from "./scrollbackBackfill.ts";
import type { SmokeApi } from "./smokeTypes.ts";

type SmokeRetainedMarkerMethods = Pick<SmokeApi, "retainedMarkerScan">;

export function createSmokeRetainedMarkerMethods(): SmokeRetainedMarkerMethods {
  return {
    async retainedMarkerScan(sessionId, prefix, pageRows = 512) {
      if (!Number.isSafeInteger(pageRows) || pageRows < 1 || pageRows > 4_096) {
        throw new Error(`invalid retained marker page size: ${pageRows}`);
      }
      const gridEpoch = cellGridEpochImpl(sessionId);
      if (!gridEpoch) throw new Error(`no cell grid epoch for ${sessionId}`);
      const escapedPrefix = prefix.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      const markerPattern = new RegExp(`${escapedPrefix}(\\d+)`, "g");
      const chunks: Array<Array<{ index: number; spans: Array<{ text: string }> }>> = [];
      let endRow = Number.MAX_SAFE_INTEGER;
      let scrollbackTotal: number | undefined;
      let retainedFloor = 0;
      // The final page carries the floor and reason established by its own clamp.
      let retainedFloorReason: ScrollbackHistoryFloor = "none";
      let pages = 0;
      for (;;) {
        if (pages >= 128) {
          throw new Error(`retained marker pagination exceeded 128 pages for ${sessionId}`);
        }
        const response = await coordClient.sessionsGetScrollbackCells({
          sessionId,
          endRow: BigInt(endRow),
          maxRows: pageRows,
          gridEpoch,
        });
        pages++;
        const responseStart = Number(response.startRow);
        const responseEnd = Number(response.endRow);
        const responseTotal = Number(response.scrollbackTotal);
        if (
          response.gridEpoch !== gridEpoch
          || !Number.isSafeInteger(responseStart)
          || !Number.isSafeInteger(responseEnd)
          || !Number.isSafeInteger(responseTotal)
          || responseStart < 0
          || responseEnd < responseStart
          || responseTotal < responseEnd
        ) {
          throw new Error(`invalid retained marker page for ${sessionId}`);
        }
        retainedFloorReason = SCROLLBACK_FLOOR_REASON[response.historyFloor] ?? "none";
        if (scrollbackTotal === undefined) {
          scrollbackTotal = responseTotal;
        } else if (scrollbackTotal !== responseTotal) {
          throw new Error(`scrollback changed during retained marker scan for ${sessionId}`);
        }
        for (let index = 0; index < response.rows.length; index++) {
          if (response.rows[index]!.index !== responseStart + index) {
            throw new Error(
              `non-contiguous retained page for ${sessionId} at ${responseStart + index}`,
            );
          }
        }
        if (response.rows.length > 0) chunks.unshift(response.rows);
        if (responseStart === 0 || response.rows.length === 0) {
          retainedFloor = responseStart;
          break;
        }
        if (responseStart >= endRow) {
          throw new Error(`retained marker page made no progress for ${sessionId}`);
        }
        endRow = responseStart;
      }

      const rows = chunks.flat();
      const rowIndices = rows.map((row) => row.index);
      let rowGapCount = 0;
      for (let index = 1; index < rowIndices.length; index++) {
        if (rowIndices[index] !== rowIndices[index - 1]! + 1) rowGapCount++;
      }
      const markerIds: number[] = [];
      for (const row of rows) {
        const text = row.spans.map((span) => span.text).join("");
        markerPattern.lastIndex = 0;
        let match: RegExpExecArray | null;
        while ((match = markerPattern.exec(text)) !== null) {
          const value = Number(match[1]);
          if (Number.isSafeInteger(value)) markerIds.push(value);
        }
      }
      const counts = new Map<number, number>();
      for (const marker of markerIds) {
        counts.set(marker, (counts.get(marker) ?? 0) + 1);
      }
      const unique = [...counts.keys()];
      const markerMin = unique.length > 0 ? Math.min(...unique) : 0;
      const markerMax = unique.length > 0 ? Math.max(...unique) : 0;
      const markerDuplicated = unique
        .filter((value) => (counts.get(value) ?? 0) > 1)
        .sort((left, right) => left - right);
      let markerMissing = 0;
      for (let value = markerMin; value <= markerMax && unique.length > 0; value++) {
        if (!counts.has(value)) markerMissing++;
      }
      let markerOutOfOrder = 0;
      for (let index = 1; index < markerIds.length; index++) {
        if (markerIds[index]! < markerIds[index - 1]!) markerOutOfOrder++;
      }
      const total = scrollbackTotal ?? 0;
      return {
        gridEpoch,
        pages,
        scrollbackTotal: total,
        retainedFloor,
        retainedCap: total - retainedFloor,
        retainedFloorReason,
        rowIndices,
        rowGapCount,
        markerIds,
        markerMin,
        markerMax,
        markerMissing,
        markerDuplicated,
        markerOutOfOrder,
      };
    },
  };
}
