import type { FixtureCase } from "../scenario";
import { fixtureCases as contentCases } from "./primitive-content";
import { fixtureCases as chartMeterCases } from "./primitive-content-chart-meter";
import { fixtureCases as codeMediaCases } from "./primitive-content-code-media";
import { fixtureCases as dataCases } from "./primitive-data";
import { fixtureCases as structuredCases } from "./primitive-content-structured";
import { fixtureCases as controlCases } from "./primitive-controls";
import { fixtureCases as editorCases } from "./primitive-editors";
import { fixtureCases as interactiveCommonCases } from "./primitive-interactive-common";
import { fixtureCases as interactiveEditorCases } from "./primitive-interactive-editor";
import { fixtureCases as interactiveListCases } from "./primitive-interactive-list";
import { fixtureCases as interactiveOverlayButtonCases } from "./primitive-interactive-overlay-button";
import { fixtureCases as timerCases } from "./primitive-timers";

export const fixtureCases: FixtureCase[] = [...contentCases, ...chartMeterCases, ...codeMediaCases, ...dataCases, ...structuredCases, ...controlCases, ...editorCases, ...interactiveCommonCases, ...interactiveEditorCases, ...interactiveListCases, ...interactiveOverlayButtonCases, ...timerCases];
