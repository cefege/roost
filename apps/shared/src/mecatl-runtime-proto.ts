// Conversion between the validated Mecatl runtime report and its protobuf
// message on the worker heartbeat. Mirrors terminal-core-capacity-proto.ts:
// the Zod schema stays the single validator, and proto carries only strings.

import { create } from "@bufbuild/protobuf";
import {
  MecatlRuntimeReportSchema as MecatlRuntimeReportProtoSchema,
  type MecatlRuntimeReport as MecatlRuntimeReportProto,
} from "./gen/roost/v1/wire_pb.ts";
import {
  MecatlRuntimeReportSchema,
  type MecatlRuntimeReport,
} from "./mecatl-runtime.ts";

export function mecatlRuntimeReportToProto(
  report: MecatlRuntimeReport,
): MecatlRuntimeReportProto {
  const checked = MecatlRuntimeReportSchema.parse(report);
  return create(MecatlRuntimeReportProtoSchema, {
    state: checked.state,
    reason: checked.reason ?? "",
  });
}

export function mecatlRuntimeReportFromProto(
  report: MecatlRuntimeReportProto,
): MecatlRuntimeReport {
  // An empty reason is the proto encoding of absent, not a value: only an
  // unavailable state carries one, and the schema rejects any other string.
  return MecatlRuntimeReportSchema.parse({
    state: report.state,
    ...(report.reason === "" ? {} : { reason: report.reason }),
  });
}
