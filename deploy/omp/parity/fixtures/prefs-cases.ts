// Index of the native prefs page fixtures; settings.ts appends these after its proof pass.
import type { FixtureCase } from "../scenario";
import { fixtureCases as advisorFixtureCases } from "./prefs-advisor";
import { fixtureCases as navFixtureCases } from "./prefs-nav";
import { fixtureCases as pagesFixtureCases } from "./prefs-pages";
import { fixtureCases as settingsFixtureCases } from "./prefs-settings";

export const prefsFixtureCases: FixtureCase[] = [...settingsFixtureCases, ...navFixtureCases, ...pagesFixtureCases, ...advisorFixtureCases];
