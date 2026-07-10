const root = document.documentElement;
const sidebar = document.querySelector("[data-sidebar]");
const menuToggle = document.querySelector("[data-menu-toggle]");
const menuClose = document.querySelector("[data-menu-close]");
const themeToggle = document.querySelector("[data-theme-toggle]");
const searchInput = document.querySelector("#docs-search");
const searchResults = document.querySelector("#search-results");
const toast = document.querySelector(".toast");
const sections = [...document.querySelectorAll(".doc-section[id]")];
const navigationLinks = [...document.querySelectorAll(".sidebar a[href^='#'], .on-this-page a[href^='#']")];

const storedTheme = localStorage.getItem("azlocaldev-docs-theme");
const preferredTheme = window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
setTheme(storedTheme || preferredTheme);

function setTheme(theme) {
  root.dataset.theme = theme;
  themeToggle.setAttribute("aria-label", theme === "dark" ? "Use light theme" : "Use dark theme");
  document.querySelector('meta[name="theme-color"]').content = theme === "dark" ? "#101715" : "#075d59";
}

themeToggle.addEventListener("click", () => {
  const nextTheme = root.dataset.theme === "dark" ? "light" : "dark";
  setTheme(nextTheme);
  localStorage.setItem("azlocaldev-docs-theme", nextTheme);
});

function setMenu(open) {
  sidebar.classList.toggle("open", open);
  menuToggle.setAttribute("aria-expanded", String(open));
  document.body.style.overflow = open ? "hidden" : "";
}

menuToggle.addEventListener("click", () => setMenu(!sidebar.classList.contains("open")));
menuClose.addEventListener("click", () => setMenu(false));
sidebar.addEventListener("click", (event) => {
  if (event.target.closest("a")) setMenu(false);
});

const searchIndex = sections.map((section) => ({
  id: section.id,
  title: section.dataset.searchTitle,
  text: section.textContent.replace(/\s+/g, " ").trim()
}));

function closeSearch() {
  searchResults.hidden = true;
  searchResults.innerHTML = "";
}

function renderSearch(query) {
  const normalizedQuery = query.trim().toLowerCase();
  if (normalizedQuery.length < 2) {
    closeSearch();
    return;
  }

  const matches = searchIndex.filter((item) =>
    item.title.toLowerCase().includes(normalizedQuery) || item.text.toLowerCase().includes(normalizedQuery)
  ).slice(0, 7);

  if (!matches.length) {
    searchResults.innerHTML = '<div class="search-empty">No matching documentation</div>';
  } else {
    searchResults.innerHTML = matches.map((item) => {
      const lowerText = item.text.toLowerCase();
      const matchIndex = lowerText.indexOf(normalizedQuery);
      const excerptStart = Math.max(0, matchIndex - 36);
      const excerpt = item.text.slice(excerptStart, excerptStart + 105);
      return `<a href="#${item.id}"><strong>${item.title}</strong><span>${excerpt}</span></a>`;
    }).join("");
  }
  searchResults.hidden = false;
}

searchInput.addEventListener("input", () => renderSearch(searchInput.value));
searchInput.addEventListener("keydown", (event) => {
  if (event.key === "Escape") {
    searchInput.blur();
    closeSearch();
  }
  if (event.key === "Enter") {
    const firstResult = searchResults.querySelector("a");
    if (firstResult) firstResult.click();
  }
});

searchResults.addEventListener("click", () => {
  searchInput.value = "";
  closeSearch();
});

document.addEventListener("click", (event) => {
  if (!event.target.closest(".search")) closeSearch();
});

document.addEventListener("keydown", (event) => {
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
    event.preventDefault();
    searchInput.focus();
  }
});

document.querySelectorAll("[data-copy-target]").forEach((button) => {
  button.addEventListener("click", async () => {
    const target = document.getElementById(button.dataset.copyTarget);
    try {
      await navigator.clipboard.writeText(target.innerText);
      button.textContent = "Copied";
      toast.hidden = false;
      toast.classList.remove("show");
      void toast.offsetWidth;
      toast.classList.add("show");
      window.setTimeout(() => {
        button.textContent = "Copy";
        toast.hidden = true;
      }, 2000);
    } catch {
      const selection = window.getSelection();
      const range = document.createRange();
      range.selectNodeContents(target);
      selection.removeAllRanges();
      selection.addRange(range);
      button.textContent = "Selected";
      window.setTimeout(() => { button.textContent = "Copy"; }, 2000);
    }
  });
});

const observer = new IntersectionObserver((entries) => {
  const visible = entries
    .filter((entry) => entry.isIntersecting)
    .sort((first, second) => second.intersectionRatio - first.intersectionRatio)[0];
  if (!visible) return;

  navigationLinks.forEach((link) => {
    link.classList.toggle("active", link.getAttribute("href") === `#${visible.target.id}`);
  });
}, { rootMargin: "-18% 0px -66%", threshold: [0, 0.1, 0.3] });

sections.forEach((section) => observer.observe(section));

function updateProgress() {
  const scrollable = document.documentElement.scrollHeight - window.innerHeight;
  const progress = scrollable > 0 ? Math.min(100, (window.scrollY / scrollable) * 100) : 0;
  document.querySelector(".page-progress i").style.width = `${progress}%`;
}

window.addEventListener("scroll", updateProgress, { passive: true });
updateProgress();