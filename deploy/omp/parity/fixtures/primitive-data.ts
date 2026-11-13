import type { FixtureCase } from "../scenario";
import { fixtureCases as checklistCases } from "./primitive-data-checklist";
import { fixtureCases as elapsedCases } from "./primitive-data-elapsed";
import { fixtureCases as contextCases } from "./primitive-data-context";
import { fixtureCases as sessionCases } from "./primitive-data-session";
import { fixtureCases as structuredCases } from "./primitive-data-structured";
import { fixtureCases as jobsCases } from "./primitive-data-jobs";
import { fixtureCases as researchCases } from "./primitive-data-research";

export const fixtureCases: FixtureCase[] = [...jobsCases, ...checklistCases, ...researchCases, ...elapsedCases, ...contextCases, ...sessionCases, ...structuredCases];
