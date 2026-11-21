/* ==========================================================================
   NEON Shared Interactive Controller — Fedora Updater Next-Gen Mockups
   Handles Theme, Narrow Viewport, GTK4 X-Ray, State Machine Switcher,
   Blueprint Spec Sync, Package Inspector, Filters, Console Drawer,
   Copy Commands, SplitButton, Selective Updates, and Live Motion Simulation.
   ========================================================================== */

(function () {
  "use strict";

  const PACKAGE_DB = {
    openssl: {
      name: "openssl",
      source: "dnf",
      badge: "DNF",
      sourceLabel: "System Package",
      oldVer: "1:3.2.2-1.fc44",
      newVer: "1:3.2.2-3.fc44",
      arch: "x86_64",
      repo: "updates",
      downloadSize: "2.1 MB",
      installDelta: "+48 KB",
      severity: "Security · Critical",
      sevClass: "sec",
      advisory: "FEDORA-2026-84a1c90f22",
      cves: ["CVE-2026-2401", "CVE-2026-2409"],
      bugzilla: "RHBZ #2349182",
      reboot: false,
      summary: "Cryptography and SSL/TLS toolkit libraries and command-line utilities",
      notes: [
        "Fixes out-of-bounds read during TLS 1.3 ALPN extension parsing (CVE-2026-2401).",
        "Fixes potential NULL pointer dereference in X.509 certificate chain verification (CVE-2026-2409).",
        "Tier 2 source: Advisory.list(availability='updates') + Rpm.list(scope='upgrades')."
      ]
    },
    kernel: {
      name: "kernel",
      source: "dnf",
      badge: "DNF",
      sourceLabel: "System Kernel",
      oldVer: "6.14.9-300.fc44",
      newVer: "6.14.11-300.fc44",
      arch: "x86_64",
      repo: "updates",
      downloadSize: "98.0 MB",
      installDelta: "+142 MB (new kernel slot)",
      severity: "Security · Important",
      sevClass: "sec",
      advisory: "FEDORA-2026-77b21e4401",
      cves: ["CVE-2026-2188"],
      bugzilla: "RHBZ #2348820",
      reboot: true,
      summary: "The Linux kernel core, drivers, and signed UEFI boot image",
      notes: [
        "Includes upstream 6.14.11 stable fixes, AMD P-State EPP tuning, and netfilter privilege fix.",
        "Triggers Restart Required via core kernel allowlist (kernel / kernel-core / kernel-modules).",
        "Installed alongside running 6.14.9 kernel; previous boot entry preserved in GRUB."
      ]
    },
    firefox: {
      name: "firefox",
      source: "dnf",
      badge: "DNF",
      sourceLabel: "System Package",
      oldVer: "139.0-1.fc44",
      newVer: "140.0-1.fc44",
      arch: "x86_64",
      repo: "updates",
      downloadSize: "112.0 MB",
      installDelta: "+3.4 MB",
      severity: "Bugfix & Stability",
      sevClass: "neutral",
      advisory: "FEDORA-2026-991c3d11a0",
      cves: [],
      bugzilla: "RHBZ #2349501",
      reboot: false,
      summary: "Mozilla Firefox Web browser with Wayland fractional scaling fixes",
      notes: [
        "Resolves PipeWire WebRTC screen-share frame drop under GNOME 48 / Mutter.",
        "Updated built-in NSS root store and WebGPU shader validation."
      ]
    },
    "gnome-shell": {
      name: "gnome-shell",
      source: "dnf",
      badge: "DNF",
      sourceLabel: "Desktop Shell",
      oldVer: "48.1-1.fc44",
      newVer: "48.2-1.fc44",
      arch: "x86_64",
      repo: "updates",
      downloadSize: "8.7 MB",
      installDelta: "+120 KB",
      severity: "Enhancement",
      sevClass: "neutral",
      advisory: "FEDORA-2026-4102b8e911",
      cves: [],
      bugzilla: "RHBZ #2349044",
      reboot: false,
      summary: "Window management and desktop user interface for GNOME",
      notes: [
        "Refines quick-settings animation timing and fixes notification badge alignment.",
        "Safe to stage live; active shell picks up new assets on next log-in."
      ]
    },
    thunderbird: {
      name: "Thunderbird",
      source: "fp",
      badge: "FP",
      sourceLabel: "Flatpak · Flathub (user)",
      oldVer: "128.0.1",
      newVer: "128.2.0",
      arch: "x86_64",
      repo: "flathub (user)",
      downloadSize: "94.0 MB",
      installDelta: "+6.8 MB",
      severity: "App Update · Extension",
      sevClass: "ext",
      advisory: "app/org.mozilla.Thunderbird/x86_64/stable",
      cves: [],
      bugzilla: "Related op: GL.nvidia-550-00-00",
      reboot: false,
      summary: "Email, calendar, and RSS client + matching NVIDIA GL driver extension",
      notes: [
        "Discovered via libflatpak list_installed_refs_for_update + ready-pre-auth resolution.",
        "Pulls related extension runtime/org.freedesktop.Platform.GL.nvidia-550-00-00 (shared bytes deduplicated)."
      ]
    },
    "uefi-dbx": {
      name: "UEFI dbx",
      source: "fw",
      badge: "FW",
      sourceLabel: "Firmware · LVFS",
      oldVer: "20240101",
      newVer: "20241101",
      arch: "uefi",
      repo: "lvfs",
      downloadSize: "18 KB",
      installDelta: "0 B (SPI flash)",
      severity: "Security · UEFI Revocation",
      sevClass: "reboot",
      advisory: "LVFS-DBX-20241101",
      cves: ["CVE-2024-7344"],
      bugzilla: "fwupd Device: 362301da643102b9f38477df",
      reboot: true,
      summary: "UEFI Secure Boot Forbidden Signature Database (dbx)",
      notes: [
        "Revokes compromised third-party UEFI bootloaders before OS boot.",
        "Pre-flight check (Tier 2 fwupd D-Bus): Battery 88% (above 25% threshold) · AC connected.",
        "Requires restart (`fwupdmgr check-reboot-needed --json` exit 0)."
      ]
    }
  };

  function getParams() {
    return new URLSearchParams(window.location.search);
  }

  function applyTheme(theme) {
    const t = theme === "light" ? "light" : "dark";
    document.documentElement.setAttribute("data-theme", t);
    try {
      localStorage.setItem("neon-fu-theme", t);
    } catch (_) {}
    document.querySelectorAll("[data-theme-toggle]").forEach((btn) => {
      btn.setAttribute("aria-pressed", t === "light" ? "true" : "false");
      btn.textContent = t === "dark" ? "Theme: Dark" : "Theme: Light";
    });
  }

  function applyNarrow(narrow) {
    const win = document.querySelector(".gtk-win");
    if (win) {
      win.classList.toggle("is-narrow", narrow);
    }
    if (narrow) {
      document.getElementById("b-inspector-wrap")?.classList.remove("open");
    }
    document.querySelectorAll("[data-narrow-toggle]").forEach((btn) => {
      btn.setAttribute("aria-pressed", narrow ? "true" : "false");
      btn.textContent = narrow ? "Viewport: Narrow 420px" : "Viewport: Desktop";
    });
  }

  function applyXray(on) {
    document.body.classList.toggle("xray-on", on);
    document.querySelectorAll("[data-xray-toggle]").forEach((btn) => {
      btn.setAttribute("aria-pressed", on ? "true" : "false");
    });
  }

  function showToast(message) {
    const toast = document.querySelector(".adw-toast");
    if (!toast) return;
    toast.textContent = message;
    toast.classList.add("show");
    clearTimeout(toast._timer);
    toast._timer = setTimeout(() => toast.classList.remove("show"), 2200);
  }

  function escapeHtml(str) {
    return String(str)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  function renderBlueprintSpec(stateId) {
    const panel = document.getElementById("neon-spec-panel");
    const specs = window.NEON_BLUEPRINTS;
    if (!panel || !specs || !specs[stateId]) return;
    const s = specs[stateId];
    panel.innerHTML = `
      <div class="spec-kicker">
        <span>1:1 GTK4 / Blueprint Spec</span>
        <span>${escapeHtml(s.phase || stateId)}</span>
      </div>
      <h3 class="spec-title">${escapeHtml(s.title)}</h3>
      <p class="spec-summary">${escapeHtml(s.summary)}</p>
      <div class="tier-badges">
        <span class="tier-badge">Tier 1: ${escapeHtml(s.tier1)}</span>
        <span class="tier-badge t2">Tier 2: ${escapeHtml(s.tier2)}</span>
      </div>
      <div class="spec-block">
        <h4>Blueprint Widget Tree</h4>
        <pre class="spec-code mono">${escapeHtml(s.blueprint)}</pre>
      </div>
      <div class="spec-block">
        <h4>Edge Cases &amp; Polish Notes</h4>
        <ul class="spec-list">
          ${(s.notes || []).map((n) => `<li>${escapeHtml(n)}</li>`).join("")}
        </ul>
      </div>
    `;
  }

  function activateState(stateId) {
    const buttons = document.querySelectorAll("[data-state-btn]");
    const panels = document.querySelectorAll("[data-state-panel]");
    if (!panels.length) return;

    const targetPanel = document.querySelector(`[data-state-panel="${stateId}"]`);
    const resolved = targetPanel ? stateId : panels[0].getAttribute("data-state-panel");

    buttons.forEach((b) => {
      b.classList.toggle("active", b.getAttribute("data-state-btn") === resolved);
    });
    panels.forEach((p) => {
      p.classList.toggle("active", p.getAttribute("data-state-panel") === resolved);
    });

    closeAllDialogs();
    renderBlueprintSpec(resolved);
    try {
      const url = new URL(window.location.href);
      url.searchParams.set("state", resolved);
      window.history.replaceState(null, "", url.toString());
    } catch (_) {}
  }

  function closeAllDialogs() {
    document.querySelectorAll(".adw-dialog-scrim.open").forEach((d) => d.classList.remove("open"));
    document.querySelectorAll(".split-menu.open").forEach((m) => m.classList.remove("open"));
  }

  function openDialog(id) {
    closeAllDialogs();
    const dlg = document.getElementById(id);
    if (dlg) dlg.classList.add("open");
  }

  function renderInspectorContent(pkg) {
    const cveHtml = pkg.cves.length
      ? pkg.cves.map((c) => `<span class="micro-tag sec">${escapeHtml(c)}</span>`).join(" ")
      : `<span class="micro-tag neutral">No CVEs</span>`;
    const rebootHtml = pkg.reboot
      ? `<span class="micro-tag reboot">Restart Required</span>`
      : `<span class="micro-tag ok">Live Safe</span>`;

    return `
      <div class="row-flex" style="align-items:flex-start; justify-content:space-between;">
        <div class="row-flex">
          <span class="src-badge ${escapeHtml(pkg.source)}">${escapeHtml(pkg.badge)}</span>
          <div>
            <div style="font-size:16px; font-weight:800;">${escapeHtml(pkg.name)}</div>
            <div class="cap dim">${escapeHtml(pkg.sourceLabel)} · ${escapeHtml(pkg.repo)}</div>
          </div>
        </div>
        ${rebootHtml}
      </div>
      <p class="cap" style="margin:0; line-height:1.5;">${escapeHtml(pkg.summary)}</p>
      <div class="adw-boxed-list">
        <div class="adw-row">
          <div class="row-main"><span class="cap dim">Installed → Target</span></div>
          <div class="ver-diff">${escapeHtml(pkg.oldVer)} → <span class="ver-new">${escapeHtml(pkg.newVer)}</span></div>
        </div>
        <div class="adw-row">
          <div class="row-main"><span class="cap dim">Download / Disk Delta</span></div>
          <div class="numeric cap" style="font-weight:700;">${escapeHtml(pkg.downloadSize)} (${escapeHtml(pkg.installDelta)})</div>
        </div>
        <div class="adw-row">
          <div class="row-main"><span class="cap dim">Advisory / Reference</span></div>
          <div class="mono cap">${escapeHtml(pkg.advisory)}</div>
        </div>
        <div class="adw-row">
          <div class="row-main"><span class="cap dim">Security &amp; Tracking</span></div>
          <div class="row-flex" style="flex-wrap:wrap; justify-content:flex-end;">${cveHtml}</div>
        </div>
      </div>
      <div class="adw-card compact">
        <div style="font-size:11.5px; font-weight:700; margin-bottom:6px;" class="dim">Release Notes &amp; Platform Provenance</div>
        <ul class="spec-list" style="margin:0; padding-left:16px;">
          ${pkg.notes.map((n) => `<li>${escapeHtml(n)}</li>`).join("")}
        </ul>
      </div>
    `;
  }

  function inspectPackage(pkgId) {
    const pkg = PACKAGE_DB[pkgId];
    if (!pkg) return;

    // Variant B inline OverlaySplitView inspector pane
    const inlinePane = document.getElementById("b-inspector-body");
    const inlineWrap = document.getElementById("b-inspector-wrap");
    if (inlinePane && inlineWrap) {
      inlinePane.innerHTML = renderInspectorContent(pkg);
      inlineWrap.classList.add("open");
      document.querySelectorAll("[data-pkg-id]").forEach((r) => {
        r.classList.toggle("is-selected", r.getAttribute("data-pkg-id") === pkgId);
      });
      return;
    }

    // Modal Adw.Dialog inspector for Variants A & C
    const dlgBody = document.getElementById("pkg-inspector-body");
    const dlgTitle = document.getElementById("pkg-inspector-title");
    if (dlgBody && dlgTitle) {
      dlgTitle.textContent = pkg.name;
      dlgBody.innerHTML = renderInspectorContent(pkg);
      openDialog("pkg-inspector-dialog");
    }
  }

  function wireFilters() {
    document.querySelectorAll("[data-filter-scope]").forEach((scope) => {
      let currentFilter = "all";
      let currentQuery = "";

      function apply() {
        const rows = scope.querySelectorAll("[data-pkg-row]");
        let visibleCount = 0;
        rows.forEach((row) => {
          const tags = (row.getAttribute("data-tags") || "").split(/\s+/);
          const text = (row.textContent || "").toLowerCase();
          const matchFilter = currentFilter === "all" || tags.includes(currentFilter);
          const matchSearch = !currentQuery || text.includes(currentQuery);
          const show = matchFilter && matchSearch;
          row.classList.toggle("hidden", !show);
          if (show) visibleCount++;
        });
        scope.querySelectorAll("[data-pkg-group]").forEach((grp) => {
          const anyVisible = grp.querySelector("[data-pkg-row]:not(.hidden)");
          grp.classList.toggle("hidden", !anyVisible);
        });
        const empty = scope.querySelector("[data-empty-filter]");
        if (empty) empty.classList.toggle("hidden", visibleCount > 0);
      }

      scope.querySelectorAll("[data-filter-btn]").forEach((btn) => {
        btn.addEventListener("click", () => {
          currentFilter = btn.getAttribute("data-filter-btn") || "all";
          scope.querySelectorAll("[data-filter-btn]").forEach((b) => {
            b.classList.toggle("active", b.getAttribute("data-filter-btn") === currentFilter);
          });
          apply();
        });
      });

      scope.querySelectorAll("[data-search-input]").forEach((inp) => {
        inp.addEventListener("input", () => {
          currentQuery = inp.value.trim().toLowerCase();
          apply();
        });
      });
    });
  }

  function wireLogFilters() {
    document.querySelectorAll("[data-log-scope]").forEach((scope) => {
      let src = "all";
      let query = "";
      const apply = () => {
        scope.querySelectorAll("[data-log-src]").forEach((line) => {
          const matchSrc = src === "all" || line.getAttribute("data-log-src") === src;
          const matchQ = !query || (line.textContent || "").toLowerCase().includes(query);
          line.classList.toggle("hidden", !(matchSrc && matchQ));
        });
      };
      scope.querySelectorAll("[data-log-filter]").forEach((btn) => {
        btn.addEventListener("click", () => {
          src = btn.getAttribute("data-log-filter") || "all";
          scope.querySelectorAll("[data-log-filter]").forEach((b) => {
            b.classList.toggle("active", b.getAttribute("data-log-filter") === src);
          });
          apply();
        });
      });
      scope.querySelectorAll("[data-log-search]").forEach((inp) => {
        inp.addEventListener("input", () => {
          query = inp.value.trim().toLowerCase();
          apply();
        });
      });
    });
  }

  function wireSelectiveCheckboxes() {
    document.querySelectorAll("[data-select-scope]").forEach((scope) => {
      const updateSummary = () => {
        const boxes = scope.querySelectorAll("[data-pkg-check]");
        let checked = 0;
        let bytesMb = 0;
        boxes.forEach((cb) => {
          if (cb.checked) {
            checked++;
            bytesMb += Number(cb.getAttribute("data-mb") || 0);
          }
        });
        const label = scope.querySelector("[data-selected-summary]");
        if (label) {
          const sizeStr = bytesMb < 1 ? `${Math.round(bytesMb * 1024)} KB` : `${bytesMb.toFixed(1).replace(/\.0$/, "")} MB`;
          label.textContent = `${checked} of ${boxes.length} selected · ${sizeStr}`;
        }
        const cta = scope.querySelector("[data-selected-cta]");
        if (cta) {
          cta.textContent = checked === boxes.length ? "Update All" : `Update ${checked} Selected`;
          cta.disabled = checked === 0;
        }
      };

      scope.querySelectorAll("[data-pkg-check]").forEach((cb) => {
        cb.addEventListener("change", updateSummary);
      });
      const toggleAll = scope.querySelector("[data-select-all]");
      if (toggleAll) {
        toggleAll.addEventListener("click", () => {
          const boxes = scope.querySelectorAll("[data-pkg-check]");
          const allChecked = Array.from(boxes).every((b) => b.checked);
          boxes.forEach((b) => (b.checked = !allChecked));
          updateSummary();
        });
      }
      updateSummary();
    });
  }

  function wireRunningSimulation() {
    document.querySelectorAll("[data-sim-running]").forEach((btn) => {
      let timer = null;
      let step = 0;
      const frames = [
        { pct: 50, itemPct: 72, item: "gnome-shell", speed: "24.8 MB/s", rem: "3 done · 3 remaining", eta: "About 2 minutes left", line: "[dnf] [4/4] Upgrading gnome-shell-48.2-1.fc44.x86_64 72%", stage: "Installing System" },
        { pct: 66, itemPct: 100, item: "gnome-shell", speed: "18.4 MB/s", rem: "4 done · 2 remaining", eta: "About 1 minute left", line: "[dnf] Complete! Verifying transaction…", stage: "Verifying System" },
        { pct: 78, itemPct: 48, item: "Thunderbird", speed: "31.2 MB/s", rem: "4 done · 2 remaining", eta: "About 1 minute left", line: "[flatpak-user] Downloading org.mozilla.Thunderbird (48%)", stage: "Updating Apps" },
        { pct: 90, itemPct: 86, item: "Thunderbird", speed: "28.9 MB/s", rem: "5 done · 1 remaining", eta: "Less than a minute left", line: "[flatpak-user] Installing org.freedesktop.Platform.GL.nvidia-550-00-00", stage: "Updating Apps" },
        { pct: 96, itemPct: 60, item: "UEFI dbx", speed: "LVFS local", rem: "5 done · 1 remaining", eta: "Less than a minute left", line: "[fwupd] Writing UEFI dbx capsule (60%)", stage: "Installing Firmware" }
      ];

      btn.addEventListener("click", () => {
        if (timer) {
          clearInterval(timer);
          timer = null;
          btn.textContent = "Simulate Live Motion";
          return;
        }
        const root = btn.closest("[data-state-panel]") || document;
        btn.textContent = "Pause Simulation";
        const applyFrame = () => {
          const f = frames[step % frames.length];
          step++;
          root.querySelectorAll("[data-sim-overall-bar]").forEach((el) => (el.style.width = `${f.pct}%`));
          root.querySelectorAll("[data-sim-overall-pct]").forEach((el) => (el.textContent = `${f.pct}%`));
          root.querySelectorAll("[data-sim-item-bar]").forEach((el) => (el.style.width = `${f.itemPct}%`));
          root.querySelectorAll("[data-sim-item-pct]").forEach((el) => (el.textContent = `${f.itemPct}%`));
          root.querySelectorAll("[data-sim-item-name]").forEach((el) => (el.textContent = f.item));
          root.querySelectorAll("[data-sim-speed]").forEach((el) => (el.textContent = f.speed));
          root.querySelectorAll("[data-sim-rem]").forEach((el) => (el.textContent = f.rem));
          root.querySelectorAll("[data-sim-eta]").forEach((el) => (el.textContent = f.eta));
          root.querySelectorAll("[data-sim-stage]").forEach((el) => (el.textContent = f.stage));
          root.querySelectorAll("[data-sim-console]").forEach((el) => (el.textContent = f.line));
          root.querySelectorAll("[data-sim-ring]").forEach((circle) => {
            const r = Number(circle.getAttribute("r") || 20);
            const c = 2 * Math.PI * r;
            circle.style.strokeDasharray = `${c.toFixed(1)}`;
            circle.style.strokeDashoffset = `${(c * (1 - f.pct / 100)).toFixed(1)}`;
          });
        };
        applyFrame();
        timer = setInterval(applyFrame, 1300);
      });
    });
  }

  document.addEventListener("DOMContentLoaded", () => {
    const params = getParams();
    let storedTheme = "dark";
    try {
      storedTheme = localStorage.getItem("neon-fu-theme") || "dark";
    } catch (_) {}
    applyTheme(params.get("theme") || storedTheme);
    applyNarrow(params.get("narrow") === "1");
    applyXray(params.get("xray") === "1");

    document.querySelectorAll("[data-theme-toggle]").forEach((btn) => {
      btn.addEventListener("click", () => {
        const cur = document.documentElement.getAttribute("data-theme") || "dark";
        applyTheme(cur === "dark" ? "light" : "dark");
      });
    });

    document.querySelectorAll("[data-narrow-toggle]").forEach((btn) => {
      btn.addEventListener("click", () => {
        const win = document.querySelector(".gtk-win");
        applyNarrow(!(win && win.classList.contains("is-narrow")));
      });
    });

    document.querySelectorAll("[data-xray-toggle]").forEach((btn) => {
      btn.addEventListener("click", () => {
        applyXray(!document.body.classList.contains("xray-on"));
      });
    });

    document.querySelectorAll("[data-state-btn]").forEach((btn) => {
      btn.addEventListener("click", () => {
        activateState(btn.getAttribute("data-state-btn"));
      });
    });

    document.querySelectorAll("[data-goto-state]").forEach((btn) => {
      btn.addEventListener("click", () => {
        activateState(btn.getAttribute("data-goto-state"));
      });
    });

    document.querySelectorAll("[data-open-dialog]").forEach((btn) => {
      btn.addEventListener("click", (e) => {
        e.stopPropagation();
        openDialog(btn.getAttribute("data-open-dialog"));
      });
    });

    document.querySelectorAll("[data-close-dialog]").forEach((btn) => {
      btn.addEventListener("click", closeAllDialogs);
    });

    document.querySelectorAll(".adw-dialog-scrim").forEach((scrim) => {
      scrim.addEventListener("click", (e) => {
        if (e.target === scrim) closeAllDialogs();
      });
    });

    document.querySelectorAll("[data-pkg-id]").forEach((row) => {
      row.addEventListener("click", (e) => {
        if (e.target.closest("input[type='checkbox']")) return;
        inspectPackage(row.getAttribute("data-pkg-id"));
      });
    });

    const closeBInspector = document.getElementById("b-inspector-close");
    if (closeBInspector) {
      closeBInspector.addEventListener("click", () => {
        document.getElementById("b-inspector-wrap")?.classList.remove("open");
      });
    }

    document.querySelectorAll("[data-expander-btn]").forEach((btn) => {
      btn.addEventListener("click", () => {
        btn.closest(".adw-expander")?.classList.toggle("open");
      });
    });

    document.querySelectorAll("[data-console-toggle]").forEach((btn) => {
      btn.addEventListener("click", () => {
        btn.closest(".adw-bottom-sheet")?.classList.toggle("open");
      });
    });

    document.querySelectorAll("[data-copy-cmd]").forEach((btn) => {
      btn.addEventListener("click", () => {
        const text = btn.getAttribute("data-copy-cmd") || "";
        if (navigator.clipboard && navigator.clipboard.writeText) {
          navigator.clipboard.writeText(text).catch(() => {});
        }
        const orig = btn.innerHTML;
        btn.innerHTML = `<span class="ok-txt">✓ Copied</span>`;
        showToast("Copied command to clipboard");
        setTimeout(() => {
          btn.innerHTML = orig;
        }, 1600);
      });
    });

    document.querySelectorAll("[data-copy-log]").forEach((btn) => {
      btn.addEventListener("click", () => {
        const pre = btn.closest(".sheet-drawer-body, [data-state-panel]")?.querySelector("pre");
        if (pre && navigator.clipboard && navigator.clipboard.writeText) {
          navigator.clipboard.writeText(pre.textContent || "").catch(() => {});
        }
        showToast("Copied log to clipboard");
      });
    });

    document.querySelectorAll("[data-toast-msg]").forEach((btn) => {
      btn.addEventListener("click", () => {
        showToast(btn.getAttribute("data-toast-msg"));
      });
    });

    document.querySelectorAll("[data-split-toggle]").forEach((btn) => {
      btn.addEventListener("click", (e) => {
        e.stopPropagation();
        const menu = btn.closest(".adw-split-btn")?.querySelector(".split-menu");
        if (menu) menu.classList.toggle("open");
      });
    });

    document.querySelectorAll(".split-menu button").forEach((item) => {
      item.addEventListener("click", () => {
        const label = item.getAttribute("data-split-label");
        const main = item.closest(".adw-split-btn")?.querySelector(".split-main");
        if (main && label) main.textContent = label;
        item.closest(".split-menu")?.classList.remove("open");
        showToast(`Mode set: ${label || "Update"}`);
      });
    });

    document.addEventListener("click", (e) => {
      if (!e.target.closest(".adw-split-btn")) {
        document.querySelectorAll(".split-menu.open").forEach((m) => m.classList.remove("open"));
      }
    });

    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape") {
        closeAllDialogs();
        return;
      }
      if (e.target.closest("input, textarea, select")) return;
      const pills = Array.from(document.querySelectorAll("[data-state-btn]"));
      if (!pills.length) return;
      const idx = pills.findIndex((p) => p.classList.contains("active"));
      if (e.key === "ArrowRight" && idx >= 0 && idx < pills.length - 1) {
        activateState(pills[idx + 1].getAttribute("data-state-btn"));
      } else if (e.key === "ArrowLeft" && idx > 0) {
        activateState(pills[idx - 1].getAttribute("data-state-btn"));
      }
    });

    wireFilters();
    wireLogFilters();
    wireSelectiveCheckboxes();
    wireRunningSimulation();

    const initialState = params.get("state");
    const firstBtn = document.querySelector("[data-state-btn]");
    if (initialState || firstBtn) {
      activateState(initialState || firstBtn.getAttribute("data-state-btn"));
    }

    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        document.documentElement.classList.add("neon-ready");
      });
    });
  });

  // Apply theme attribute immediately before first paint when possible
  try {
    const p = new URLSearchParams(window.location.search);
    const t = p.get("theme") || localStorage.getItem("neon-fu-theme") || "dark";
    document.documentElement.setAttribute("data-theme", t === "light" ? "light" : "dark");
  } catch (_) {}
})();
