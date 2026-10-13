import { motion, AnimatePresence } from "framer-motion";
import { useEffect, useState } from "react";
import { ArrowUpRight } from "lucide-react";
import { EASE } from "./ui";
import { copy, REPO, RELEASES, type Lang } from "../i18n";

function GithubIcon({ size = 17 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
      <path d="M15 22v-4a4.8 4.8 0 0 0-1-3.5c3 0 6-2 6-5.5.08-1.25-.27-2.48-1-3.5.28-1.15.28-2.35 0-3.5 0 0-1 0-3 1.5-2.64-.5-5.36-.5-8 0C6 2 5 2 5 2c-.3 1.15-.3 2.35 0 3.5A5.403 5.403 0 0 0 4 9c0 3.5 3 5.5 6 5.5-.39.49-.68 1.05-.85 1.65-.17.6-.22 1.23-.15 1.85v4" />
      <path d="M9 18c-4.51 2-5-2-7-2" />
    </svg>
  );
}

/* -------------------------------- Preloader -------------------------------- */

export function Preloader({ done, lang }: { done: () => void; lang: Lang }) {
  const c = copy[lang];
  const [count, setCount] = useState(0);
  const [out, setOut] = useState(false);

  useEffect(() => {
    let raf = 0;
    const t0 = performance.now();
    const D = 1300;
    const tick = (t: number) => {
      const p = Math.min(1, (t - t0) / D);
      const eased = 1 - Math.pow(1 - p, 3);
      setCount(Math.round(eased * 100));
      if (p < 1) raf = requestAnimationFrame(tick);
      else {
        setTimeout(() => setOut(true), 220);
        setTimeout(done, 1050);
      }
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [done]);

  return (
    <AnimatePresence>
      {!out ? (
        <motion.div
          className="fixed inset-0 z-[100] flex flex-col items-center justify-center bg-[#F3F0E9]"
          exit={{ y: "-100%", transition: { duration: 0.85, ease: EASE } }}
          key="loader"
        >
          <div className="overflow-hidden">
            <motion.div
              initial={{ y: "110%" }}
              animate={{ y: "0%" }}
              transition={{ duration: 0.9, ease: EASE }}
              className="flex items-baseline gap-3"
            >
              <span className="font-display text-[13vw] md:text-[6vw] leading-none tracking-tight">Hikari</span>
              <span className="font-song text-[4vw] md:text-[1.4vw] text-[#8b8880]">光</span>
            </motion.div>
          </div>
          <motion.p
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            transition={{ delay: 0.45, duration: 0.8 }}
            className={`mt-4 text-[#8b8880] ${lang === "zh" ? "font-song text-base tracking-[0.2em]" : "font-mono text-[11px] tracking-[0.35em] uppercase"}`}
          >
            {c.loaderLine}
          </motion.p>
          <div className="mt-10 w-[min(300px,60vw)]">
            <div className="mb-3 flex justify-between font-mono text-[10px] tracking-[0.25em] uppercase text-[#8b8880]">
              <span>{c.loaderMeta}</span>
              <span>{String(count).padStart(3, "0")}</span>
            </div>
            <div className="h-px w-full bg-black/10">
              <div className="h-px bg-black transition-[width] duration-100" style={{ width: `${count}%` }} />
            </div>
          </div>
        </motion.div>
      ) : (
        <motion.div
          key="curtain"
          className="fixed inset-0 z-[100] bg-[#F3F0E9]"
          initial={{ y: 0 }}
          animate={{ y: "-100%" }}
          transition={{ duration: 0.85, ease: EASE }}
        />
      )}
    </AnimatePresence>
  );
}

/* ----------------------------------- Nav ----------------------------------- */

export function Nav({
  ready,
  lang,
  setLang,
}: {
  ready: boolean;
  lang: Lang;
  setLang: (l: Lang) => void;
}) {
  const c = copy[lang];
  const [scrolled, setScrolled] = useState(false);
  useEffect(() => {
    const on = () => setScrolled(window.scrollY > 40);
    window.addEventListener("scroll", on, { passive: true });
    return () => window.removeEventListener("scroll", on);
  }, []);

  return (
    <motion.header
      initial={{ y: -32, opacity: 0 }}
      animate={ready ? { y: 0, opacity: 1 } : {}}
      transition={{ duration: 1, ease: EASE, delay: 0.15 }}
      className="fixed inset-x-0 top-0 z-[80] mix-blend-difference text-[#F3F0E9]"
    >
      <div
        className={`mx-auto flex max-w-[1400px] items-center justify-between px-6 md:px-10 transition-all duration-500 ${
          scrolled ? "py-4" : "py-6"
        }`}
      >
        <a href="#home" className="group flex items-baseline gap-2">
          <span className="font-display text-2xl leading-none tracking-tight">Hikari</span>
          <span className="font-song text-sm opacity-60 transition-opacity group-hover:opacity-100">光</span>
        </a>

        <div className="flex items-center gap-5 md:gap-7">
          {/* language switch */}
          <div className="flex items-center gap-2.5 font-mono text-[11px] tracking-[0.18em]" role="group" aria-label="Language">
            <button
              onClick={() => setLang("en")}
              className={`py-1 transition-all duration-300 ${
                lang === "en" ? "opacity-100 underline underline-offset-[5px] decoration-1" : "opacity-40 hover:opacity-80"
              }`}
            >
              EN
            </button>
            <span className="opacity-30">/</span>
            <button
              onClick={() => setLang("zh")}
              className={`py-1 font-sans text-[12px] tracking-[0.1em] transition-all duration-300 ${
                lang === "zh" ? "opacity-100 underline underline-offset-[5px] decoration-1" : "opacity-40 hover:opacity-80"
              }`}
            >
              中文
            </button>
          </div>

          <a
            href={REPO}
            target="_blank"
            rel="noreferrer"
            className="hidden opacity-70 transition-all duration-300 hover:rotate-12 hover:opacity-100 sm:block"
            aria-label="GitHub"
          >
            <GithubIcon size={17} />
          </a>

          <a
            href={RELEASES}
            target="_blank"
            rel="noreferrer"
            className="group flex items-center gap-1.5 rounded-full border border-white/25 px-4 py-1.5 font-mono text-[11px] tracking-[0.18em] uppercase transition-all duration-500 hover:bg-white hover:text-black"
          >
            {c.download}
            <ArrowUpRight size={13} className="transition-transform group-hover:translate-x-0.5 group-hover:-translate-y-0.5" />
          </a>
        </div>
      </div>
    </motion.header>
  );
}
