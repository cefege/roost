import { expect, test } from "bun:test";
import {
  collectHostIdentity,
  type HostIdentitySources,
} from "../src/host-identity.ts";

function sources(values: {
  readonly darwin?: Record<string, string | null>;
  readonly osRelease?: string | null;
  readonly windowsModel?: string | null;
}): HostIdentitySources {
  return {
    readDarwinSysctl: (name) => values.darwin?.[name] ?? null,
    readLinuxOsRelease: () => values.osRelease ?? null,
    readWindowsModel: () => values.windowsModel ?? null,
  };
}

test("collects a Mac model and only a verified Apple chip", () => {
  const identity = collectHostIdentity("darwin", sources({
    darwin: {
      "hw.model": "MacBookPro18,3\n",
      "machdep.cpu.brand_string": "Apple M1 Pro\n",
    },
  }));

  expect(identity).toEqual({
    hardware_model: "MacBookPro18,3",
    chip: "Apple M1 Pro",
    linux_distribution: null,
  });
});

test("does not report an Intel macOS CPU as an Apple chip", () => {
  const identity = collectHostIdentity("darwin", sources({
    darwin: {
      "hw.model": "Macmini8,1",
      "machdep.cpu.brand_string": "Intel(R) Core(TM) i7-8700B CPU @ 3.20GHz",
    },
  }));

  expect(identity).toEqual({
    hardware_model: "Macmini8,1",
    chip: null,
    linux_distribution: null,
  });
});

test("reads Linux distribution from local os-release and bounds display text", () => {
  const identity = collectHostIdentity("linux", sources({
    osRelease: `NAME="Fedora Linux"\nPRETTY_NAME="Fedora Linux ${"x".repeat(300)}"\n`,
  }));

  expect(identity?.hardware_model).toBeNull();
  expect(identity?.chip).toBeNull();
  expect(identity?.linux_distribution?.startsWith("Fedora Linux ")).toBe(true);
  expect(Buffer.byteLength(identity?.linux_distribution ?? "", "utf8"))
    .toBeLessThanOrEqual(256);
});

test("falls back to NAME when PRETTY_NAME is unavailable", () => {
  expect(collectHostIdentity("linux", sources({
    osRelease: 'NAME="Alpine Linux"\n',
  }))).toEqual({
    hardware_model: null,
    chip: null,
    linux_distribution: "Alpine Linux",
  });
});

test("collects Windows hardware model without a CPU identity", () => {
  expect(collectHostIdentity("win32", sources({
    windowsModel: "Surface Laptop 7",
  }))).toEqual({
    hardware_model: "Surface Laptop 7",
    chip: null,
    linux_distribution: null,
  });
});
