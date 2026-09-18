// The compact deck bar's count-badge numbering (lib/deckTabBadge.ts): the
// 1-based position/total fraction, and every case that must fall back to the
// bare total instead of printing a nonsense position.

import { describe, test, expect } from "bun:test";
import { deckTabBadge } from "../src/lib/deckTabBadge.ts";

describe("deckTabBadge", () => {
  test("a lone terminal stays a bare number", () => {
    expect(deckTabBadge(1, 0)).toEqual({
      text: "1",
      description: "1 terminal in this workspace",
      fraction: false,
    });
  });

  test("first of two reads 1/2", () => {
    expect(deckTabBadge(2, 0)).toEqual({
      text: "1/2",
      description: "terminal 1 of 2 in this workspace",
      fraction: true,
    });
  });

  test("last of two reads 2/2", () => {
    expect(deckTabBadge(2, 1)).toEqual({
      text: "2/2",
      description: "terminal 2 of 2 in this workspace",
      fraction: true,
    });
  });

  test("last of five reads 5/5", () => {
    expect(deckTabBadge(5, 4)).toEqual({
      text: "5/5",
      description: "terminal 5 of 5 in this workspace",
      fraction: true,
    });
  });

  test("unknown active terminal (findIndex -1) falls back to the total", () => {
    expect(deckTabBadge(5, -1)).toEqual({
      text: "5",
      description: "5 terminals in this workspace",
      fraction: false,
    });
  });

  test("index past the end never prints 6/5", () => {
    expect(deckTabBadge(5, 5)).toEqual({
      text: "5",
      description: "5 terminals in this workspace",
      fraction: false,
    });
  });

  test("empty list", () => {
    expect(deckTabBadge(0, -1)).toEqual({
      text: "0",
      description: "0 terminals in this workspace",
      fraction: false,
    });
  });
});
