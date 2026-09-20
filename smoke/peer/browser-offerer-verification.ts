// Browser raw-message verification for the finite native runtime qualification.
// qualify-runtime invokes this after BrowserOfferer returns bounded raw payload receipts.
// Packet frames bypass this file and are verified by packet-qualification.ts.

import {
  QUALIFICATION_CHANNELS,
  checksumHex,
  qualificationFailure,
  type QualificationGeneration,
} from "./qualification-common.ts";
import type { BrowserReceivedPayloads } from "./browser-offerer.ts";

export function verifyBrowserReceivedMessages(
  received: BrowserReceivedPayloads,
  expectedMessages: readonly Uint8Array[],
  generation: QualificationGeneration,
): void {
  if (received.channels.length !== QUALIFICATION_CHANNELS.length) {
    throw qualificationFailure("browser-messages", generation, "browser_channel_count_mismatch");
  }
  for (const channelPayloads of received.channels) {
    if (channelPayloads.length !== expectedMessages.length) {
      throw qualificationFailure("browser-messages", generation, "browser_message_count_mismatch");
    }
    for (let index = 0; index < expectedMessages.length; index++) {
      const expected = expectedMessages[index]!;
      const bytes = Buffer.from(channelPayloads[index]!, "base64");
      if (bytes.byteLength !== expected.byteLength || checksumHex(bytes) !== checksumHex(expected)) {
        throw qualificationFailure("browser-messages", generation, "browser_message_checksum_mismatch");
      }
    }
  }
}
