const notes = {
  0: `<p><strong>Idle.</strong> Entry point. <em>Check for Updates</em> triggers polkit elevation, then <code>dnf check-update --refresh</code>.</p>`,
  1: `<p><strong>Auth (polkit).</strong> Immediate step after Check. System polkit dialog collects the password — this app never handles credentials. In-app UI only waits / allows Cancel.</p>`,
  2: `<p><strong>Checking.</strong> Elevated <code>dnf check-update --refresh</code>. Console collapsed to one live line.</p>`,
  3: `<p><strong>Ready.</strong> Transaction preview. Compact header + breathing room for actions. <em>Update All</em> may re-prompt polkit if the auth session expired, then runs <code>dnf update -y</code>.</p>`,
  4: `<p><strong>Running.</strong> Burn-down: completed section stays <em>collapsed</em> by default; active row has progress; console = 1 live line. Header typography matches Ready.</p>`,
  5: `<p><strong>Done.</strong> Summary + reboot card. <em>Restart Now</em> runs <code>systemctl reboot</code>. Close the app anytime instead.</p>`,
};

const nav = document.getElementById("screen-nav");
const screens = document.querySelectorAll(".screen");
const notesEl = document.getElementById("notes-content");

function showScreen(index) {
  screens.forEach((s) => {
    s.classList.toggle("hidden", Number(s.dataset.screen) !== index);
  });
  nav.querySelectorAll("button").forEach((b) => {
    b.classList.toggle("active", Number(b.dataset.screen) === index);
  });
  notesEl.innerHTML = notes[index] || "";
  history.replaceState(null, "", `#screen-${index}`);
}

nav.addEventListener("click", (e) => {
  const btn = e.target.closest("button[data-screen]");
  if (!btn) return;
  showScreen(Number(btn.dataset.screen));
});

const panel = document.getElementById("console-panel");
const toggle = document.getElementById("console-toggle");
const body = document.getElementById("console-body");
if (toggle && panel && body) {
  toggle.addEventListener("click", () => {
    const expanded = panel.classList.toggle("expanded");
    body.classList.toggle("hidden", !expanded);
  });
}

const hash = location.hash.match(/screen-(\d+)/);
showScreen(hash ? Number(hash[1]) : 0);
