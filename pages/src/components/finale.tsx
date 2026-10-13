import { motion, useScroll, useTransform } from "framer-motion";
import { useRef, useState } from "react";
import { ArrowDown, ArrowUpRight, Check, Copy } from "lucide-react";
import { FadeUp, MaskLines } from "./ui";
import { copy, RELEASES, REPO, type Lang } from "../i18n";

export function Finale({ lang }: { lang: Lang }) {
  const c = copy[lang];
  const zh = lang === "zh";
  const ref = useRef<HTMLElement>(null);
  const { scrollYProgress } = useScroll({ target: ref, offset: ["start end", "end end"] });
  const yWord = useTransform(scrollYProgress, [0, 1], [90, 0]);
  const [copied, setCopied] = useState(false);

  const doCopy = async () => {
    try {
      await navigator.clipboard.writeText("cargo run");
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
    } catch {
      /* noop */
    }
  };

  const rows = [
    { title: c.dlTitle, desc: c.dlDesc, href: RELEASES, icon: ArrowDown },
    { title: c.srcTitle, desc: c.srcDesc, href: REPO, icon: ArrowUpRight },
  ];

  return (
    <section ref={ref} id="begin" className="snap-start relative overflow-hidden">
      <div className="mx-auto flex min-h-[100svh] w-full max-w-[1400px] flex-col px-6 pb-8 pt-28 md:px-10 md:pt-32">
        {/* label */}
        <FadeUp>
          <div className="flex items-center gap-4 font-mono text-[11px] tracking-[0.3em] uppercase text-[#8b8880]">
            <span>( 03 )</span>
            <span className="h-px w-12 bg-black/20" />
            <span className={zh ? "font-song text-sm tracking-[0.4em]" : ""}>{c.beginLabel}</span>
          </div>
        </FadeUp>

        {/* headline */}
        <div className="mt-10 md:mt-14">
          <MaskLines
            key={`big-${lang}`}
            lines={[
              <span key="a" className={`font-display leading-[1.0] tracking-tight text-[13vw] md:text-[5.4vw] ${zh ? "font-medium" : ""}`}>
                {c.beginL1}
              </span>,
            ]}
          />
          <FadeUp delay={0.25}>
            <p className={`mt-6 ${zh ? "font-song text-xl text-[#3a3935]" : "text-[15px] font-light text-[#8b8880]"}`}>{c.beginSub}</p>
          </FadeUp>
        </div>

        {/* jump rows */}
        <div className="mt-12 md:mt-16">
          {rows.map((r, i) => (
            <FadeUp key={r.title} delay={i * 0.08}>
              <a
                href={r.href}
                target="_blank"
                rel="noreferrer"
                className="group flex items-center justify-between gap-6 border-t border-black/[0.12] py-6 md:py-7"
              >
                <span className="min-w-0">
                  <span className="block font-display text-4xl tracking-tight transition-transform duration-500 ease-out group-hover:translate-x-3 md:text-6xl">
                    {r.title}
                  </span>
                  <span className={`mt-2 block max-w-md text-sm font-light leading-relaxed text-[#8b8880] ${zh ? "font-song text-[15px]" : ""}`}>
                    {r.desc}
                  </span>
                </span>
                <span className="flex h-11 w-11 shrink-0 items-center justify-center rounded-full border border-black/20 transition-all duration-500 group-hover:rotate-45 group-hover:border-black group-hover:bg-black group-hover:text-[#F3F0E9] md:h-12 md:w-12">
                  <r.icon size={17} />
                </span>
              </a>
            </FadeUp>
          ))}

          {/* cargo run */}
          <FadeUp delay={0.16}>
            <button onClick={doCopy} className="group flex w-full items-center justify-between gap-6 border-b border-t border-black/[0.12] py-6 text-left md:py-7">
              <span className="min-w-0">
                <span className="block font-mono text-[22px] tracking-tight transition-transform duration-500 ease-out group-hover:translate-x-3 md:text-4xl">
                  <span className="text-[#b9b6ad]">$ </span>cargo run
                  <span className="animate-blink ml-2 inline-block h-[0.55em] w-[0.45em] translate-y-[0.05em] bg-black/70" />
                </span>
                <span className={`mt-2 block max-w-md text-sm font-light leading-relaxed text-[#8b8880] ${zh ? "font-song text-[15px]" : ""}`}>
                  {c.runDesc}
                </span>
              </span>
              <span
                className={`flex h-11 w-11 shrink-0 items-center justify-center rounded-full border transition-all duration-500 md:h-12 md:w-12 ${
                  copied ? "border-black bg-black text-[#F3F0E9]" : "border-black/20 group-hover:border-black"
                }`}
              >
                {copied ? <Check size={17} /> : <Copy size={17} />}
              </span>
            </button>
          </FadeUp>
        </div>

        {/* giant word */}
        <div className="mt-auto select-none overflow-hidden pt-14 md:pt-16">
          <motion.div style={{ y: yWord }} className="flex items-baseline justify-center gap-4 md:gap-6">
            <span className="font-display leading-[0.82] tracking-[-0.03em] text-[21vw] md:text-[15.5vw]">Hikari</span>
            <span className="font-song text-[7vw] text-[#8b8880] md:text-[4.2vw]">光</span>
          </motion.div>
        </div>

        {/* footer */}
        <div className="mt-10 flex flex-col items-center justify-between gap-3 border-t border-black/[0.12] pt-6 font-mono text-[10px] tracking-[0.2em] uppercase text-[#8b8880] md:flex-row">
          <span>
            © 2026 Hikari <span className="font-song">光</span> · {c.footNote}
          </span>
          <span className="hidden font-sans text-xs font-light normal-case tracking-normal lg:block">{c.footCredit}</span>
          <a href="#home" className="u-link">
            {c.backTop} ↑
          </a>
        </div>
      </div>
    </section>
  );
}
