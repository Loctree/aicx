const copyButtons = document.querySelectorAll(".copy-button");

for (const button of copyButtons) {
  button.addEventListener("click", async () => {
    const text = button.dataset.copy ?? "";
    try {
      await navigator.clipboard.writeText(text);
      const previous = button.textContent;
      button.textContent = "Copied";
      window.setTimeout(() => {
        button.textContent = previous;
      }, 1400);
    } catch {
      button.textContent = "Copy failed";
    }
  });
}

const observer = new IntersectionObserver(
  (entries) => {
    for (const entry of entries) {
      if (!entry.isIntersecting) continue;
      entry.target.classList.add("is-visible");
      observer.unobserve(entry.target);
    }
  },
  {
    threshold: 0.18,
  }
);

document.querySelectorAll(".fade-up").forEach((element) => observer.observe(element));
