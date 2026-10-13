import { useState } from "react";
import { Moon, Sun, Plus, Search, PanelLeft, Settings, FolderOpen, SquarePen, ArrowUp } from "lucide-react";
import { FadeUp } from "./ui";
import { copy, type Lang } from "../i18n";

export function Interface({ lang }: { lang: Lang }) {
  const c = copy[lang];
  const [dark, setDark] = useState(true);
  const zh = lang === "zh";

  return (
    <section id="interface" className="dark-block snap-start relative overflow-hidden bg-[#0b0b0a] text-[#F3F0E9]">
      <div className="relative mx-auto flex min-h-[100svh] w-full max-w-[1200px] flex-col justify-center px-6 py-24 md:px-10">
        {/* header row */}
        <FadeUp className="mb-10 flex items-end justify-between md:mb-12">
          <div className="flex items-center gap-4 font-mono text-[11px] tracking-[0.3em] uppercase text-white/40">
            <span>( 02 )</span>
            <span className="h-px w-10 bg-white/20" />
            <span className={zh ? "font-song text-sm tracking-[0.4em]" : ""}>{c.intLabel}</span>
          </div>

          {/* theme toggle */}
          <div className="flex items-center gap-1 rounded-full border border-white/15 p-1 font-mono text-[10px] tracking-[0.25em] uppercase">
            {(
              [
                { mode: false, icon: Sun, label: c.light },
                { mode: true, icon: Moon, label: c.dark },
              ] as const
            ).map(({ mode, icon: Icon, label }) => (
              <button
                key={label}
                onClick={() => setDark(mode)}
                className={`flex items-center gap-2 rounded-full px-4 py-1.5 transition-all duration-500 ${
                  dark === mode ? "bg-[#F3F0E9] text-black" : "text-white/50 hover:text-white"
                }`}
              >
                <Icon size={12} />
                <span className={zh ? "font-song text-xs tracking-[0.3em]" : ""}>{label}</span>
              </button>
            ))}
          </div>
        </FadeUp>

        {/* window */}
        <FadeUp delay={0.1}>
          <div
            className={`overflow-hidden rounded-2xl border text-left shadow-[0_50px_140px_-30px_rgba(0,0,0,0.85)] transition-colors duration-700 ${
              dark ? "border-white/10 bg-[#111311]" : "border-black/10 bg-[#FAF9F6]"
            }`}
          >
            {/* title bar */}
            <div className={`flex items-center justify-between px-5 py-3.5 ${dark ? "text-white/70" : "text-black/70"}`}>
              <div className="flex items-center gap-3">
                <div className="flex gap-1.5">
                  {[0, 1, 2].map((d) => (
                    <span key={d} className={`h-2.5 w-2.5 rounded-full ${dark ? "bg-white/15" : "bg-black/10"}`} />
                  ))}
                </div>
                <span className="ml-2 font-sans text-[13px] font-medium tracking-wide">Hikari</span>
              </div>
              <div className={`flex items-center gap-4 ${dark ? "text-white/40" : "text-black/40"}`}>
                <SquarePen size={14} />
                <FolderOpen size={14} />
                <span className="font-mono text-xs">—</span>
                <span className="font-mono text-xs">▢</span>
                <span className="font-mono text-xs">✕</span>
              </div>
            </div>

            <div className="grid min-h-[430px] grid-cols-1 sm:grid-cols-[220px_1fr] md:min-h-[500px]">
              {/* sidebar */}
              <div className={`hidden flex-col border-r px-4 py-4 sm:flex ${dark ? "border-white/[0.07] bg-white/[0.02]" : "border-black/[0.07] bg-black/[0.02]"}`}>
                <div className="flex items-center justify-between px-2">
                  <span className={`text-[13px] font-medium ${dark ? "text-white/85" : "text-black/85"}`}>Hikari</span>
                  <div className={`flex gap-3 ${dark ? "text-white/40" : "text-black/40"}`}>
                    <Search size={13} />
                    <PanelLeft size={13} />
                  </div>
                </div>
                <div className={`mt-4 flex items-center gap-2 rounded-lg px-2.5 py-2 text-[13px] ${dark ? "text-white/80" : "text-black/80"}`}>
                  <Plus size={13} /> {c.winNew}
                </div>
                <div className={`mt-3 px-2.5 text-[11px] ${dark ? "text-white/30" : "text-black/30"}`}>{c.winLocal}</div>
                <div className="flex-1" />
                <div className={`flex items-center gap-4 border-t px-2.5 pt-3 ${dark ? "border-white/[0.07] text-white/40" : "border-black/[0.07] text-black/40"}`}>
                  <Settings size={14} />
                  {dark ? <Sun size={14} /> : <Moon size={14} />}
                </div>
              </div>

              {/* main */}
              <div className="relative flex flex-col px-5 py-8 md:px-10">
                <div className="flex flex-1 flex-col items-center justify-center text-center">
                  <img
                    src="./images/portrait.png"
                    alt="Hikari"
                    className={`h-20 w-20 rounded-full object-cover transition-all duration-700 ${dark ? "invert" : ""}`}
                  />
                  <h3 className={`mt-6 font-song text-2xl font-semibold md:text-[28px] ${dark ? "text-white" : "text-black"}`}>
                    {c.winGreeting}
                  </h3>
                  <p className={`mt-2.5 ${zh ? "font-song text-sm text-[#8b8880]" : "font-mono text-[10px] tracking-[0.3em] uppercase text-[#8b8880]"}`}>
                    {c.winSub}
                  </p>
                </div>

                {/* input (visual) */}
                <div className={`mt-6 rounded-2xl border p-4 transition-colors duration-500 ${dark ? "border-white/10 bg-white/[0.05]" : "border-black/10 bg-white"}`}>
                  <div className={`flex items-center text-[14px] font-light ${dark ? "text-white/35" : "text-black/35"}`}>
                    <span>{c.winPlaceholder}</span>
                    <span className="animate-blink ml-1 inline-block h-4 w-[7px] bg-current opacity-70" />
                  </div>
                  <div className="mt-4 flex items-center justify-between">
                    <div className={`flex items-center gap-3 font-mono text-[11px] ${dark ? "text-white/45" : "text-black/45"}`}>
                      <Plus size={13} />
                      <span className={`rounded-full px-2.5 py-1 ${dark ? "bg-white/[0.08]" : "bg-black/[0.05]"}`}>{c.winModel}</span>
                    </div>
                    <span className={`flex h-8 w-8 items-center justify-center rounded-full ${dark ? "bg-white text-black" : "bg-black text-white"}`}>
                      <ArrowUp size={15} />
                    </span>
                  </div>
                </div>
              </div>
            </div>
          </div>
        </FadeUp>

        {/* caption */}
        <FadeUp delay={0.2}>
          <p
            key={lang}
            className={`mt-10 text-center text-white/45 ${
              zh ? "font-song text-lg tracking-[0.08em]" : "font-display text-xl italic md:text-2xl"
            }`}
          >
            {c.intCaption}
          </p>
        </FadeUp>
      </div>
    </section>
  );
}
