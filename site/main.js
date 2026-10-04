// herder.sh: theme cycling (T, or the theme button) and the install command's copy button.
(() => {
  const themes = [
    ["tokyo-night", "tokyo night", "#1a1b26"],
    ["catppuccin", "catppuccin", "#1e1e2e"],
    ["gruvbox", "gruvbox", "#282828"],
    ["nord", "nord", "#2e3440"],
    ["rose-pine", "rosé pine", "#191724"],
    ["flexoki-light", "flexoki light", "#fffcf0"],
  ];
  const root = document.documentElement;
  const nameEl = document.querySelector(".theme-name");
  const metaColor = document.querySelector('meta[name="theme-color"]');
  const toast = document.querySelector(".toast");
  let toastTimer;

  const say = (text) => {
    toast.textContent = text;
    toast.classList.add("on");
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => toast.classList.remove("on"), 1600);
  };

  const apply = (index, announce) => {
    const [id, label, bg] = themes[index];
    root.dataset.theme = id;
    nameEl.textContent = label;
    metaColor.setAttribute("content", bg);
    try { localStorage.setItem("herder-theme", id); } catch (e) { /* private mode */ }
    if (announce) say(`theme: ${label}`);
  };

  let current = Math.max(0, themes.findIndex(([id]) => id === root.dataset.theme));
  apply(current, false);

  const next = () => {
    current = (current + 1) % themes.length;
    apply(current, true);
  };

  document.querySelector("[data-theme-next]").addEventListener("click", next);
  document.addEventListener("keydown", (e) => {
    if (e.key !== "t" && e.key !== "T") return;
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.target.closest("input, textarea, [contenteditable]")) return;
    next();
  });

  document.querySelectorAll("[data-copy]").forEach((button) => {
    button.addEventListener("click", async () => {
      const text = document.getElementById(button.dataset.copy).textContent;
      try {
        await navigator.clipboard.writeText(text);
      } catch (e) {
        const range = document.createRange();
        range.selectNodeContents(document.getElementById(button.dataset.copy));
        getSelection().removeAllRanges();
        getSelection().addRange(range);
        return;
      }
      const label = button.querySelector("span");
      label.textContent = "copied";
      button.classList.add("done");
      setTimeout(() => { label.textContent = "copy"; button.classList.remove("done"); }, 1600);
    });
  });
})();
