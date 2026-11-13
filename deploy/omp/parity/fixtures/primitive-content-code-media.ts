import type { FixtureCase } from "../scenario";
import { fixtureCases as messageCases } from "./primitive-content-messages";
import { fixtureCases as toolCases } from "./primitive-content-tools";
import { fixtureCases as cardCases } from "./primitive-content-cards";
import { fixtureCases as replyCases } from "./primitive-content-reply";
import { fixtureCases as gitCases } from "./primitive-content-git";

export const fixtureCases: FixtureCase[] = [
  ...toolCases,
  ...messageCases,
  ...cardCases,
  ...replyCases,
  ...gitCases,
];
