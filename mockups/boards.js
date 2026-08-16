const nav = document.getElementById("studio-nav");
const themeBtn = document.getElementById("theme-toggle");
const notesBtn = document.getElementById("notes-toggle");

function setTheme(theme) {
  document.documentElement.dataset.theme = theme;
  localStorage.setItem("fu-boards-theme", theme);
  if (themeBtn) {
    themeBtn.setAttribute("aria-pressed", theme === "dark" ? "true" : "false");
    themeBtn.textContent = theme === "dark" ? "Dark" : "Light";
  }
}

function setNotesFree(on) {
  document.documentElement.classList.toggle("notes-free", on);
  localStorage.setItem("fu-boards-notes-free", on ? "1" : "0");
  if (notesBtn) {
    notesBtn.setAttribute("aria-pressed", on ? "true" : "false");
    notesBtn.textContent = on ? "Notes off" : "Notes on";
  }
}

function showBoard(id) {
  document.querySelectorAll(".board").forEach((b) => {
    b.hidden = b.id !== id;
  });
  nav?.querySelectorAll("a").forEach((a) => {
    a.classList.toggle("active", a.getAttribute("href") === `#${id}`);
  });
  // One board is visible; stay at the top so the sticky header cannot cover it.
  requestAnimationFrame(() => window.scrollTo(0, 0));
}

const params = new URLSearchParams(location.search);
const initialTheme =
  params.get("theme") ||
  localStorage.getItem("fu-boards-theme") ||
  (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
setTheme(initialTheme === "dark" ? "dark" : "light");
setNotesFree(
  params.get("notes") === "off" ||
    (params.get("notes") !== "on" && localStorage.getItem("fu-boards-notes-free") === "1")
);

const hash = (location.hash || "#flow").replace("#", "");
showBoard(document.getElementById(hash) ? hash : "flow");
window.addEventListener("load", () => window.scrollTo(0, 0));

nav?.addEventListener("click", (e) => {
  const a = e.target.closest("a[href^='#']");
  if (!a) return;
  e.preventDefault();
  const id = a.getAttribute("href").slice(1);
  showBoard(id);
  history.replaceState(null, "", `#${id}`);
});

themeBtn?.addEventListener("click", () => {
  setTheme(document.documentElement.dataset.theme === "dark" ? "light" : "dark");
});

notesBtn?.addEventListener("click", () => {
  setNotesFree(!document.documentElement.classList.contains("notes-free"));
});

document.addEventListener("click", (e) => {
  const consoleToggle = e.target.closest(".console-toggle");
  if (consoleToggle) {
    consoleToggle.closest(".console-bar")?.classList.toggle("open");
    return;
  }
  const completed = e.target.closest(".completed > button");
  if (completed) {
    completed.parentElement.classList.toggle("open");
  }
});
