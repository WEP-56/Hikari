import { motion, useScroll, useTransform } from "framer-motion";
import { useRef } from "react";
import { ArrowDown, ArrowUpRight } from "lucide-react";
import { EASE, SplitChars } from "./ui";
import { copy, RELEASES, REPO, type Lang } from "../i18n";

export function Hero({ ready, lang }: { ready: boolean; lang: Lang }) {
  const c = copy[lang];
  const ref = useRef<HTMLDivElement>(null);
  const { scrollYProgress } = useScroll({ target: ref, offset: ["start start", "end start"] });
  const yTitle = useTransform(scrollYProgress, [0, 1], [0, 160]);
  const opacity = useTransform(scrollYProgress, [0, 0.75], [1, 0]);

  const zh = lang === "zh";

  return (
    <section ref={ref} id="home" className="snap-start relative flex min-h-[100svh] flex-col overflow-hidden">
      {/* breathing light */}
      <div className="pointer-events-none absolute left-1/2 top-[40%] -translate-x-1/2 -translate-y-1/2">
        <div className="animate-breathe h-[62vmin] w-[62vmin] rounded-full bg-[radial-gradient(circle,rgba(11,11,10,0.07)_0%,transparent_65%)]" />
      </div>

      {/* side vertical marks */}
      <motion.div
        initial={{ opacity: 0 }}
        animate={ready ? { opacity: 1 } : {}}
        transition={{ delay: 1.1, duration: 1.2 }}
        className="vertical-rl absolute left-6 top-1/2 hidden -translate-y-1/2 font-mono text-[10px] uppercase text-[#b9b6ad] md:left-10 md:block"
      >
        ヒカリ — Hikari — 光
      </motion.div>
      <motion.div
        initial={{ opacity: 0 }}
        animate={ready ? { opacity: 1 } : {}}
        transition={{ delay: 1.1, duration: 1.2 }}
        className={`vertical-rl absolute right-6 top-1/2 hidden -translate-y-1/2 rotate-180 text-[#b9b6ad] md:right-10 md:block ${
          zh ? "font-song text-[11px]" : "font-mono text-[10px] uppercase"
        }`}
      >
        {c.heroEyebrow}
      </motion.div>

      <motion.div style={{ y: yTitle, opacity }} className="relative z-10 flex flex-1 flex-col items-center justify-center px-6 pb-16 pt-28 text-center">
        {/* eyebrow */}
        <motion.div
          initial={{ opacity: 0, y: 16 }}
          animate={ready ? { opacity: 1, y: 0 } : {}}
          transition={{ duration: 0.9, ease: EASE, delay: 0.2 }}
          className={`mb-10 flex items-center gap-3 text-[#8b8880] ${
            zh ? "font-song text-sm tracking-[0.45em]" : "font-mono text-[10px] tracking-[0.4em] uppercase md:text-[11px]"
          }`}
        >
          <span className="inline-block h-1 w-1 rounded-full bg-black" />
          {c.heroEyebrow}
          <span className="inline-block h-1 w-1 rounded-full bg-black" />
        </motion.div>

        {/* portrait */}
        <motion.div
          initial={{ opacity: 0, scale: 0.88, filter: "blur(10px)" }}
          animate={ready ? { opacity: 1, scale: 1, filter: "blur(0px)" } : {}}
          transition={{ duration: 1.3, ease: EASE, delay: 0.35 }}
          className="overflow-hidden rounded-full"
        >
          <img src="./images/portrait.png" alt="Hikari" className="h-24 w-24 object-cover md:h-28 md:w-28" />
        </motion.div>
        <motion.div
          initial={{ scaleX: 0 }}
          animate={ready ? { scaleX: 1 } : {}}
          transition={{ duration: 1.1, ease: EASE, delay: 0.8 }}
          className="mx-auto mb-10 mt-6 h-px w-10 bg-black/30"
        />

        {/* headline */}
        <h1 className={`font-display leading-[1.04] tracking-[-0.01em] text-[12.5vw] md:text-[6.8vw] ${zh ? "font-medium" : ""}`}>
          <span className="block overflow-hidden pb-[0.08em]">
            {ready ? (
              <SplitChars key={`l1-${lang}`} text={c.heroL1} delay={0.15} stagger={zh ? 0.055 : 0.02} as="span" className="block" />
            ) : (
              <span className="block opacity-0">{c.heroL1}</span>
            )}
          </span>
          <span className="block overflow-hidden pb-[0.12em]">
            {ready ? (
              <span key={`l2-${lang}`} className="block">
                <SplitChars text={c.heroL2a} delay={0.5} stagger={zh ? 0.06 : 0.02} as="span" />
                <SplitChars text={c.heroL2b} delay={0.75} stagger={zh ? 0.06 : 0.028} as="span" className={zh ? "" : "italic"} />
              </span>
            ) : (
              <span className="block opacity-0">{c.heroL2a + c.heroL2b}</span>
            )}
          </span>
        </h1>

        {/* sub */}
        <motion.p
          key={`sub-${lang}`}
          initial={{ opacity: 0, y: 16 }}
          animate={ready ? { opacity: 1, y: 0 } : {}}
          transition={{ duration: 1, ease: EASE, delay: 1.15 }}
          className={`mt-9 max-w-xl ${zh ? "font-song text-lg text-[#3a3935]" : "text-[15px] font-light leading-relaxed text-[#8b8880]"}`}
        >
          {c.heroSub}
        </motion.p>

        {/* actions — text only */}
        <motion.div
          initial={{ opacity: 0, y: 16 }}
          animate={ready ? { opacity: 1, y: 0 } : {}}
          transition={{ duration: 1, ease: EASE, delay: 1.35 }}
          className="mt-12 flex items-center gap-10"
        >
          <a
            href={RELEASES}
            target="_blank"
            rel="noreferrer"
            className="group relative overflow-hidden rounded-full bg-[#0b0b0a] px-8 py-3.5 font-mono text-[12px] tracking-[0.2em] uppercase text-[#F3F0E9] transition-transform duration-500 hover:scale-[1.03] active:scale-[0.98]"
          >
            <span className="relative z-10 flex items-center gap-2">
              {c.download}
              <ArrowDown size={14} className="transition-transform duration-500 group-hover:translate-y-0.5" />
            </span>
            <span className="absolute inset-0 -translate-x-full bg-[#2a2a27] transition-transform duration-500 ease-out group-hover:translate-x-0" />
          </a>
          <a
            href={REPO}
            target="_blank"
            rel="noreferrer"
            className="u-link group flex items-center gap-1.5 font-mono text-[12px] tracking-[0.2em] uppercase text-[#0b0b0a]"
          >
            {c.heroSource}
            <ArrowUpRight size={14} className="transition-transform duration-300 group-hover:translate-x-0.5 group-hover:-translate-y-0.5" />
          </a>
        </motion.div>
      </motion.div>

      {/* bottom meta */}
      <motion.div
        initial={{ opacity: 0 }}
        animate={ready ? { opacity: 1 } : {}}
        transition={{ delay: 1.7, duration: 1 }}
        className={`relative z-10 mx-auto flex w-full max-w-[1400px] items-end justify-between px-6 pb-8 md:px-10 ${
          zh ? "font-song text-xs text-[#8b8880]" : "font-mono text-[10px] tracking-[0.25em] uppercase text-[#8b8880]"
        }`}
      >
        <span className="hidden sm:block">{c.heroMetaL}</span>
        <span className="flex flex-col items-center gap-3">
          <span className={zh ? "" : "font-mono text-[10px] tracking-[0.25em] uppercase"}>{c.scrollHint}</span>
          <span className="animate-scroll-line block h-12 w-px bg-[#0b0b0a]" />
        </span>
        <span className="hidden sm:block">{c.heroMetaR}</span>
      </motion.div>
    </section>
  );
}
