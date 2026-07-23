(() => {
  const root = document.documentElement;
  const intro = document.querySelector(".intro");

  if (!root.classList.contains("will-intro") || !intro) return;

  const body = document.body;
  const output = intro.querySelector(".terminal-typed");
  const message = intro.dataset.message || "dowse search";
  let finished = false;

  const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

  const finish = async () => {
    if (finished) return;
    finished = true;
    try { sessionStorage.setItem("dowse:intro:v1", "1"); } catch (_) {}

    body.classList.add("intro-finished");
    intro.classList.add("is-leaving");
    await wait(680);
    root.classList.remove("will-intro");
    body.classList.remove("intro-active", "intro-finished");
    intro.remove();
  };

  const play = async () => {
    body.classList.add("intro-active");
    await wait(180);
    intro.classList.add("is-ready");
    await wait(520);

    for (let index = 0; index < message.length && !finished; index += 1) {
      output.textContent += message[index];
      await wait(message[index] === " " ? 34 : 48);
    }

    if (finished) return;
    intro.classList.add("is-complete");
  };

  addEventListener("keydown", (event) => {
    if (event.key === "Escape" || event.key === "Enter" || event.key === " ") finish();
  }, { once: true });
  intro.addEventListener("click", finish, { once: true });

  play();
})();
