// Machine identity presentation remains pinned to worker-derived static fields.
// The mapping must not infer a model or Linux distribution from a mutable label.

import { describe, expect, test } from "bun:test";
import { machineIdentityPresentation } from "../src/lib/machineIdentity.ts";

describe("machineIdentityPresentation", () => {
  test("renders verified MacBook and Apple chip identity", () => {
    expect(machineIdentityPresentation({
      os: "darwin",
      host_identity: {
        hardware_model: "MacBookPro18,3",
        chip: "Apple M3 Pro",
        linux_distribution: null,
      },
    })).toMatchObject({
      icon: "laptop_mac",
      label: "MacBook",
      appleChipBadge: "M3 Pro",
    });
  });

  test("uses a local distro mark only for recognized worker identity", () => {
    expect(machineIdentityPresentation({
      os: "linux",
      host_identity: {
        hardware_model: null,
        chip: null,
        linux_distribution: "Ubuntu 24.04.3 LTS",
      },
    })).toMatchObject({ label: "Ubuntu Linux", linuxBrand: "ubuntu" });

    expect(machineIdentityPresentation({
      os: "linux",
      host_identity: {
        hardware_model: null,
        chip: null,
        linux_distribution: "Kestrel OS",
      },
    })).toMatchObject({ label: "Linux", linuxBrand: null });
  });

  test("distinguishes worker-reported Windows laptops from desktops", () => {
    expect(machineIdentityPresentation({
      os: "win32",
      host_identity: {
        hardware_model: "Surface Laptop 7",
        chip: null,
        linux_distribution: null,
      },
    })).toMatchObject({ icon: "laptop_windows", label: "Windows laptop" });

    expect(machineIdentityPresentation({
      os: "win32",
      host_identity: {
        hardware_model: "Surface Studio 2",
        chip: null,
        linux_distribution: null,
      },
    })).toMatchObject({ icon: "desktop_windows", label: "Windows PC" });
  });
});
