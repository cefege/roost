// Direct scrollback keeps its authorization predicate live across the authoritative
// reader await. A grant reduction after read admission must return no row payload
// even when the reader completed a valid page before the reply reached history.

import { create } from "@bufbuild/protobuf";
import { afterEach, expect, test } from "bun:test";
import { LocalScrollbackRequestSchema } from "@roost/protocol/proto/local_terminal_pb";
import { readLocalScrollback } from "../../src/local-door/local-terminal-scrollback.ts";
import { installAutoKeeper } from "../keeper-fake-pool.ts";
import {
	cleanupStreamHarnesses,
	makeHarness,
	SESSION_ID,
	TEST_COLS,
	TEST_ROWS,
	trackKeeper,
} from "../terminal/terminal-stream-state-harness.ts";

afterEach(cleanupStreamHarnesses);

test("post-read authority loss suppresses the direct scrollback page", async () => {
	trackKeeper(installAutoKeeper({ cols: TEST_COLS, rows: TEST_ROWS }));
	const harness = await makeHarness();
	let authorizationChecks = 0;
	const response = await readLocalScrollback(
		harness.manager,
		create(LocalScrollbackRequestSchema, {
			requestId: "history-authority-loss",
			sessionId: SESSION_ID,
			gridEpoch: "",
			endRow: 0n,
			maxRows: 1,
		}),
		() => {
			authorizationChecks += 1;
			return authorizationChecks === 1;
		},
	);

	expect(authorizationChecks).toBe(2);
	expect(response).toMatchObject({
		requestId: "history-authority-loss",
		error: "terminal session is unavailable",
		rows: [],
	});
});
