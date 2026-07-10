document.documentElement.classList.add("js");

const header = document.querySelector("[data-header]");
const menuButton = document.querySelector("[data-menu-toggle]");
const mobileMenu = document.querySelector("[data-mobile-menu]");
const toast = document.querySelector(".toast");

function renderIcons(container = document) {
  if (window.lucide) window.lucide.createIcons({ attrs: { "stroke-width": 1.8 }, root: container });
}

renderIcons();

function setMenu(open) {
  mobileMenu.hidden = !open;
  menuButton.setAttribute("aria-expanded", String(open));
  menuButton.setAttribute("aria-label", open ? "Close navigation" : "Open navigation");
  document.body.classList.toggle("menu-open", open);

  const icon = menuButton.querySelector("svg");
  if (icon) {
    icon.outerHTML = `<i data-lucide="${open ? "x" : "menu"}" aria-hidden="true"></i>`;
    renderIcons(menuButton);
  }
}

menuButton.addEventListener("click", () => {
  setMenu(menuButton.getAttribute("aria-expanded") !== "true");
});

mobileMenu.addEventListener("click", (event) => {
  if (event.target.closest("a")) setMenu(false);
});

window.addEventListener("resize", () => {
  if (window.innerWidth > 980 && !mobileMenu.hidden) setMenu(false);
});

function updateHeader() {
  header.classList.toggle("scrolled", window.scrollY > 12);
}

window.addEventListener("scroll", updateHeader, { passive: true });
updateHeader();

const revealObserver = new IntersectionObserver((entries, observer) => {
  entries.forEach((entry) => {
    if (!entry.isIntersecting) return;
    entry.target.classList.add("visible");
    observer.unobserve(entry.target);
  });
}, { rootMargin: "0px 0px -8%", threshold: 0.08 });

document.querySelectorAll(".reveal").forEach((element) => revealObserver.observe(element));

function showToast(message) {
  toast.textContent = message;
  toast.hidden = false;
  toast.classList.remove("show");
  void toast.offsetWidth;
  toast.classList.add("show");
  window.setTimeout(() => { toast.hidden = true; }, 2000);
}

document.querySelectorAll("[data-copy-target]").forEach((button) => {
  button.addEventListener("click", async () => {
    const target = document.getElementById(button.dataset.copyTarget);
    if (!target) return;

    try {
      await navigator.clipboard.writeText(target.innerText);
      const label = button.querySelector("span");
      if (label) label.textContent = "Copied";
      showToast("Commands copied to clipboard");
      window.setTimeout(() => { if (label) label.textContent = "Copy"; }, 2000);
    } catch {
      const selection = window.getSelection();
      const range = document.createRange();
      range.selectNodeContents(target);
      selection.removeAllRanges();
      selection.addRange(range);
      showToast("Commands selected");
    }
  });
});

function formatBytes(bytes) {
  const megabytes = bytes / (1024 * 1024);
  return `${megabytes.toFixed(megabytes >= 10 ? 0 : 1)} MB ZIP`;
}

async function loadLatestRelease() {
  try {
    const response = await fetch("https://api.github.com/repos/BipulRaman/AzLocalDev/releases/latest", {
      headers: { Accept: "application/vnd.github+json" }
    });
    if (!response.ok) return;

    const release = await response.json();
    const asset = release.assets.find((item) => item.name === "AzLocalDev-x86_64-pc-windows-msvc.zip");
    const version = release.tag_name.startsWith("v") ? release.tag_name : `v${release.tag_name}`;

    document.querySelectorAll("[data-release-label]").forEach((element) => {
      element.textContent = `${version} available`;
    });
    document.querySelectorAll("[data-release-version]").forEach((element) => {
      element.textContent = `${version} stable`;
    });
    document.querySelectorAll("[data-release-heading]").forEach((element) => {
      element.textContent = `Az.Local.Dev ${version}`;
    });

    if (!asset) return;
    document.querySelectorAll("[data-release-size]").forEach((element) => {
      element.textContent = formatBytes(asset.size);
    });
    document.querySelectorAll("[data-download-link]").forEach((link) => {
      link.href = asset.browser_download_url;
    });
  } catch {
    // Static latest-release links remain fully functional when the API is unavailable.
  }
}

loadLatestRelease();