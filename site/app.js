import { html, render, useState, useEffect, useRef } from "./preact-htm.js";

const pad = (number) => String(number).padStart(2, "0");

const steps = [
  {
    title: "The expensive model writes the task",
    body: "It spells out what's needed, the edge cases, and which commands have to pass.",
  },
  {
    title: "A cheaper model does the work",
    body: "It runs in a separate copy of your repo, so your own checkout isn't touched.",
  },
  {
    title: "You get a diff to look at",
    body: "If you like, a reviewer checks it first and sends problems back to be fixed.",
  },
];

const messages = [
  { author: "Claude", text: "Limit logins to 5 a minute per IP. The tests need to pass.", mine: true },
  { author: "Worker", text: "Done. I added the limiter and 9 tests." },
  { author: "Reviewer", text: "The limit resets too early. Try a sliding window." },
  { author: "Worker", text: "Fixed, and there's a test for it now." },
];

const diffLines = [
  { text: "  function rateLimit(opts) {" },
  { text: "-   const b = minuteOf(now())", kind: "del" },
  { text: "+   const since = now() - opts.ms", kind: "add" },
  { text: "+   const hits = store.after(ip)", kind: "add" },
  { text: "+   if (hits.length >= opts.max)", kind: "add" },
  { text: "+     return tooMany(hits)", kind: "add" },
  { text: "+   store.add(ip, now())", kind: "add" },
  { text: "  }" },
];

const commands = [
  { note: "# Install it", prompt: "$", text: "cargo install gate" },
  { note: "# Turn it on in a project", prompt: ">", text: "/gate-enable" },
  { note: "# Hand off a task", prompt: ">", text: "/gate-flow add rate limiting to login" },
  { note: "# Watch it work", prompt: "$", text: "gate web" },
];

const Logo = () => html`<svg viewBox="0 0 221 86.5" aria-hidden="true"><use href="#mark" /></svg>`;

const Mark = () => html`
  <svg width="0" height="0" style="position:absolute" aria-hidden="true">
    <symbol id="mark" viewBox="0 0 221 86.5">
      <path d="M39.86896 0.25456c9.58536-1.35538 21.90547 2.86566 29.43594 8.76361 10.2254 8.00865 14.94824 17.76843 16.60645 30.43802l45.49727 0.00078c-1.79102-1.88785-3.89649-3.61925-5.83596-5.36059-2.58984-2.32362-5.47654-4.64784-4.03512-8.5253 0.52928-1.43272 1.6035-2.59885 2.98631-3.24558 1.34961-0.61549 2.91406-0.61061 4.28711-0.06953 0.62305 0.25177 1.20703 0.59515 1.72851 1.01883 2.80274 2.27733 6.35936 5.9292 9.08594 8.43818 2.01367 1.85307 9.76563 8.71301 10.51367 10.52077 0.6543 1.54193 0.65038 3.28464-0.00975 4.82384-0.23048 0.5354-0.70314 1.31143-1.11917 1.72396-6.0742 6.02882-12.1582 12.07951-18.37889 17.95615-2.92969 2.70686-7.84764 1.53354-9.13476-2.40077-1.66796-5.1047 4.62304-8.39072 7.44334-11.65524 0.53906-0.62311 1.37502-1.17686 1.75977-1.9152l-0.31054-0.12365c-14.78318-0.24846-30.17988-0.03183-45.00801-0.05098-0.99843 7.22813-4.46855 14.41426-9.04883 20.04324-7.16113 8.75325-17.49512 14.31639-28.74452 15.47431-11.49591 1.20067-22.99356-2.25527-31.92265-9.59533-8.82071-7.17383-14.38438-17.59518-15.4379-28.91641-1.18282-11.47809 2.28652-22.95108 9.63124-31.85025 7.67091-9.23024 18.13888-14.39395 30.01055-15.49286z" />
      <path transform="translate(139.96 .15)" d="M34.90039 0.11276c2.59766-0.31409 7.46484 0.08185 10.04297 0.52741 11.30469 1.9693 21.36133 8.35086 27.96094 17.73932 6.49805 9.26385 8.99805 21.33026 7.01953 32.43918-1.98047 11.1597-8.32227 21.07182-17.62305 27.54872-9.42773 6.64957-21.14063 9.21579-32.48633 7.11836-12.21484-2.23575-21.61133-9.00015-28.59765-19.11719 2.85547-2.6801 5.66406-5.4106 8.42187-8.18992 3.05664 5.26625 5.7207 8.20242 10.73827 11.60153 0.7383 0.50647 1.40431 0.90144 2.18556 1.33018 7.31055 3.96321 15.88867 4.87715 23.86914 2.54259 8.16211-2.41231 15.01172-8.00241 19.01172-15.51454 3.99219-7.49768 4.84375-16.27301 2.36914-24.39847-2.41406-7.96374-7.88672-14.64554-15.2168-18.5816-7.40625-4.01868-16.10742-4.92364-24.18164-2.51485-9.52539 2.89633-15.25391 8.75052-19.79492 17.28755-2.87695-2.61935-5.75194-5.38327-8.61914-8.03583 1.3828-2.47012 3.00781-4.79648 4.85156-6.9451 7.75391-9.02808 18.26367-13.96207 30.04883-14.83734z" />
    </symbol>
  </svg>`;

const Header = () => html`
  <header>
    <a class="brand" href="#what-it-is"><${Logo} />Gate</a>
    <nav aria-label="Site">
      <a href="https://github.com/randheer-rk/gate/tree/main/docs">Docs</a>
      <a href="https://github.com/randheer-rk/gate">GitHub</a>
    </nav>
  </header>`;

const Handoff = () => html`
  <div class="handoff">
    <div class="card">
      <span class="label">Plans and reviews</span>
      <h3>Claude Code or Codex</h3>
      <p>Writes the task, then reads the finished diff.</p>
    </div>
    <${Logo} />
    <div class="card worker">
      <span class="label">Does the work</span>
      <h3>A cheaper model</h3>
      <p>Reads the files, writes the code, runs the tests.</p>
    </div>
  </div>`;

const Bars = () => html`
  <div class="bars">
    <figure>
      <div class="bar"><div style="flex:1">Expensive model does everything</div></div>
      <figcaption>Without Gate</figcaption>
    </figure>
    <figure>
      <div class="bar">
        <div style="height:56px">Plan</div>
        <div class="cheap" style="flex:1">Coding, on a cheaper model</div>
        <div style="height:56px">Review</div>
      </div>
      <figcaption>With Gate</figcaption>
    </figure>
  </div>
  <span class="note">A sketch of where the work goes, not a measured result.</span>`;

const Steps = () => html`
  <ol class="steps card">
    ${steps.map(
      (step, index) => html`
        <li>
          <span class="n">${index + 1}</span>
          <div><h3>${step.title}</h3><p>${step.body}</p></div>
        </li>
      `,
    )}
  </ol>`;

const Diff = () => html`
  <div class="diff">
    <div class="diff-head"><span>rateLimit.ts</span><span class="add">+42</span><span class="del">−6</span></div>
    <pre>${diffLines.map((line) => html`<span class=${line.kind}>${line.text}</span>`)}</pre>
  </div>`;

const WebView = () => html`
  <div
    class="window card"
    role="img"
    aria-label="Example of Gate's web page: a conversation between Claude, a worker and a reviewer, next to the code changes."
  >
    <div class="window-bar"><i></i><i></i><i></i>localhost:7777</div>
    <div class="window-body">
      <div class="chat">
        ${messages.map(
          (message) => html`
            <div class=${message.mine ? "msg me" : "msg"}>
              <b>${message.author}</b>
              <span>${message.text}</span>
            </div>
          `,
        )}
      </div>
      <${Diff} />
    </div>
  </div>`;

const Terminal = () => {
  const rows = commands.map(
    (command) => html`<div><small>${command.note}</small><code data-p=${command.prompt}>${command.text}</code></div>`,
  );
  return html`<pre class="terminal">${rows}</pre>`;
};

const Copy = ({ number, title, children }) => {
  const kicker = `${pad(number)} · ${title}`;
  return html`
    <div class="copy">
      <p class="kicker">${kicker}</p>
      ${children}
    </div>`;
};

const Slide = ({ id, title, number, copy, visual }) => html`
  <section class="slide" id=${id} aria-label=${title}>
    <${Copy} number=${number} title=${title}>${copy}</${Copy}>
    <div class="visual">${visual}</div>
  </section>`;

const slides = [
  {
    id: "what-it-is",
    title: "What it is",
    copy: html`
      <h1>Spend fewer Claude and Codex tokens by letting a cheaper model write the code.</h1>
      <p>Claude Code and Codex are good at planning and reviewing, but they're expensive to run on every file read and edit. Gate lets them hand the actual coding to a cheaper model and check the result afterwards.</p>
      <div class="actions">
        <span class="cmd"><span>$</span>cargo install gate</span>
        <a class="btn" href="https://github.com/randheer-rk/gate#setup">Read the docs</a>
      </div>
    `,
    visual: html`<${Handoff} />`,
  },
  {
    id: "why",
    title: "Why it saves tokens",
    copy: html`
      <h2>Most of the tokens in a coding task go on reading files and writing edits.</h2>
      <p>When one model does everything, you pay its full price for every file it opens and every line it writes. With Gate, the expensive model only writes the task and reads the final diff. The long middle part runs on something cheaper.</p>
    `,
    visual: html`<${Bars} />`,
  },
  {
    id: "how",
    title: "How it works",
    copy: html`
      <h2>Three steps, and you only need to be there for the last one.</h2>
      <p>You ask Claude or Codex for something as usual. Gate takes care of passing it along and bringing the result back.</p>
    `,
    visual: html`<${Steps} />`,
  },
  {
    id: "see",
    title: "What you can see",
    copy: html`
      <h2>You can watch the whole thing as it happens.</h2>
      <p>Gate has a small local web page. It shows what the models say to each other and the changes as they're made, so you're never guessing what the cheaper model is up to.</p>
    `,
    visual: html`<${WebView} />`,
  },
  {
    id: "start",
    title: "Getting started",
    copy: html`
      <h2>It takes a few minutes to set up.</h2>
      <p>You'll need Rust, OpenCode running on your machine, and either Claude Code or Codex. Gate only switches on in the projects where you ask for it.</p>
      <div class="actions"><a class="btn" href="https://github.com/randheer-rk/gate">View on GitHub</a></div>
    `,
    visual: html`<${Terminal} />`,
  },
];

const Controls = ({ current, onGo }) => {
  const count = `${pad(current + 1)} / ${pad(slides.length)}`;
  return html`
    <footer>
      <span class="count" aria-live="polite">${count}</span>
      <div class="dots" role="tablist" aria-label="Slides">
        ${slides.map(
          (slide, index) => html`
            <button
              aria-label=${`Slide ${index + 1}: ${slide.title}`}
              aria-current=${index === current ? "true" : "false"}
              onClick=${() => onGo(index)}
            ></button>
          `,
        )}
      </div>
      <div class="arrows">
        <button aria-label="Previous slide" disabled=${current === 0} onClick=${() => onGo(current - 1)}>
          <svg viewBox="0 0 24 24"><path d="M19 12H5M11 6l-6 6 6 6" /></svg>
        </button>
        <button aria-label="Next slide" disabled=${current === slides.length - 1} onClick=${() => onGo(current + 1)}>
          <svg viewBox="0 0 24 24"><path d="M5 12h14M13 6l6 6-6 6" /></svg>
        </button>
      </div>
    </footer>`;
};

const keyTargets = {
  ArrowRight: (current) => current + 1,
  PageDown: (current) => current + 1,
  ArrowLeft: (current) => current - 1,
  PageUp: (current) => current - 1,
  Home: () => 0,
  End: () => slides.length - 1,
};

const App = () => {
  const trackRef = useRef(null);
  const [current, setCurrent] = useState(0);

  const go = (index) => {
    const track = trackRef.current;
    const clamped = Math.max(0, Math.min(slides.length - 1, index));
    track.children[clamped].scrollIntoView({ inline: "start", block: "nearest" });
  };

  const isStacked = () => getComputedStyle(trackRef.current).display === "block";

  useEffect(() => {
    const track = trackRef.current;
    let frame = 0;
    const sync = () => {
      frame = 0;
      setCurrent(Math.round(track.scrollLeft / track.clientWidth));
    };
    const onScroll = () => {
      frame = requestAnimationFrame(sync);
    };
    track.addEventListener("scroll", onScroll, { passive: true });
    sync();
    return () => {
      track.removeEventListener("scroll", onScroll);
      cancelAnimationFrame(frame);
    };
  }, []);

  useEffect(() => {
    const onKeyDown = (event) => {
      if (isStacked()) return;
      const target = keyTargets[event.key];
      if (!target) return;
      event.preventDefault();
      go(target(current));
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [current]);

  useEffect(() => {
    const onResize = () => {
      if (!isStacked()) go(current);
    };
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [current]);

  return html`
    <${Mark} />
    <${Header} />
    <main
      class="slides"
      id="slides"
      tabindex="0"
      aria-roledescription="carousel"
      aria-label="About Gate"
      ref=${trackRef}
    >
      ${slides.map(
        (slide, index) => html`
          <${Slide} id=${slide.id} title=${slide.title} number=${index + 1} copy=${slide.copy} visual=${slide.visual} />
        `,
      )}
    </main>
    <${Controls} current=${current} onGo=${go} />
  `;
};

render(html`<${App} />`, document.body);
