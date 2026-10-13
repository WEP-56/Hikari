import { motion, useScroll, useTransform, MotionValue } from "framer-motion";
import { useRef, type ReactNode } from "react";

export const EASE = [0.22, 1, 0.36, 1] as const;

/* ---------------------------------- Split ---------------------------------- */

export function SplitChars({
  text,
  className = "",
  delay = 0,
  stagger = 0.018,
  once = true,
  as: Tag = "span",
}: {
  text: string;
  className?: string;
  delay?: number;
  stagger?: number;
  once?: boolean;
  as?: "span" | "div" | "p" | "h1" | "h2";
}) {
  const chars = text.split("");
  const MTag = motion[Tag] as typeof motion.span;
  return (
    <MTag
      className={className}
      initial="hidden"
      whileInView="visible"
      viewport={{ once, margin: "-12% 0px" }}
      aria-label={text}
    >
      {chars.map((c, i) => (
        <motion.span
          key={i}
          className="inline-block will-change-transform"
          style={{ whiteSpace: c === " " ? "pre" : undefined }}
          variants={{
            hidden: { y: "110%", opacity: 0, filter: "blur(6px)", rotate: 2 },
            visible: {
              y: "0%",
              opacity: 1,
              filter: "blur(0px)",
              rotate: 0,
              transition: {
                duration: 0.9,
                ease: EASE,
                delay: delay + i * stagger,
              },
            },
          }}
          aria-hidden
        >
          {c === " " ? "\u00A0" : c}
        </motion.span>
      ))}
    </MTag>
  );
}

export function MaskLines({
  lines,
  className = "",
  lineClassName = "",
  delay = 0,
  stagger = 0.12,
}: {
  lines: ReactNode[];
  className?: string;
  lineClassName?: string;
  delay?: number;
  stagger?: number;
}) {
  return (
    <div className={className}>
      {lines.map((line, i) => (
        <span key={i} className="mask-line pb-[0.08em] -mb-[0.08em]">
          <motion.span
            className={lineClassName}
            initial={{ y: "112%", opacity: 0 }}
            whileInView={{ y: "0%", opacity: 1 }}
            viewport={{ once: true, margin: "-10% 0px" }}
            transition={{ duration: 1.1, ease: EASE, delay: delay + i * stagger }}
          >
            {line}
          </motion.span>
        </span>
      ))}
    </div>
  );
}

/* --------------------------------- FadeUp ---------------------------------- */

export function FadeUp({
  children,
  className = "",
  delay = 0,
  y = 28,
  duration = 1,
}: {
  children: ReactNode;
  className?: string;
  delay?: number;
  y?: number;
  duration?: number;
}) {
  return (
    <motion.div
      className={className}
      initial={{ opacity: 0, y, filter: "blur(6px)" }}
      whileInView={{ opacity: 1, y: 0, filter: "blur(0px)" }}
      viewport={{ once: true, margin: "-8% 0px" }}
      transition={{ duration, ease: EASE, delay }}
    >
      {children}
    </motion.div>
  );
}

/* ------------------------------ Section label ------------------------------ */

export function SectionLabel({
  index,
  en,
  zh,
  dark = false,
}: {
  index: string;
  en: string;
  zh: string;
  dark?: boolean;
}) {
  return (
    <FadeUp>
      <div
        className={`flex items-center gap-4 font-mono text-[11px] tracking-[0.3em] uppercase ${
          dark ? "text-[#8b8880]" : "text-[#8b8880]"
        }`}
      >
        <span className="text-current">({index})</span>
        <span className={`h-px w-12 ${dark ? "bg-white/20" : "bg-black/20"}`} />
        <span>{en}</span>
        <span className="font-song tracking-[0.5em]">{zh}</span>
      </div>
    </FadeUp>
  );
}

/* ------------------------- Scroll-linked word reveal ------------------------ */

export function ScrollWords({
  text,
  className = "",
  wordClassName = "",
}: {
  text: string;
  className?: string;
  wordClassName?: string;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const { scrollYProgress } = useScroll({
    target: ref,
    offset: ["start 0.82", "end 0.42"],
  });
  const words = text.split(" ");
  return (
    <div ref={ref} className={className}>
      {words.map((w, i) => (
        <Word key={i} progress={scrollYProgress} range={[i / words.length, (i + 1) / words.length]}>
          <span className={wordClassName}>{w}</span>
        </Word>
      ))}
    </div>
  );
}

function Word({
  children,
  progress,
  range,
}: {
  children: ReactNode;
  progress: MotionValue<number>;
  range: [number, number];
}) {
  const opacity = useTransform(progress, range, [0.12, 1]);
  const y = useTransform(progress, range, [8, 0]);
  const blur = useTransform(progress, range, [4, 0]);
  const filter = useTransform(blur, (b) => `blur(${b}px)`);
  return (
    <motion.span
      className="inline-block mr-[0.28em] will-change-transform"
      style={{ opacity, y, filter }}
    >
      {children}
    </motion.span>
  );
}

/* ------------------------------- Hairline grow ------------------------------ */

export function GrowLine({ dark = false, className = "" }: { dark?: boolean; className?: string }) {
  return (
    <motion.div
      className={`${dark ? "bg-white/15" : "bg-black/15"} h-px w-full ${className}`}
      initial={{ scaleX: 0 }}
      whileInView={{ scaleX: 1 }}
      viewport={{ once: true, margin: "-10% 0px" }}
      transition={{ duration: 1.4, ease: EASE }}
      style={{ transformOrigin: "left" }}
    />
  );
}
