import { useEffect, useState } from "react";
import { Preloader, Nav } from "./components/chrome";
import { Hero } from "./components/hero";
import { Interface } from "./components/interface";
import { Finale } from "./components/finale";
import type { Lang } from "./i18n";

function detectLang(): Lang {
  if (typeof navigator !== "undefined" && navigator.language.toLowerCase().startsWith("zh")) return "zh";
  return "en";
}

export default function App() {
  const [loading, setLoading] = useState(true);
  const [ready, setReady] = useState(false);
  const [lang, setLang] = useState<Lang>(detectLang);

  useEffect(() => {
    document.body.style.overflow = loading ? "hidden" : "";
    return () => {
      document.body.style.overflow = "";
    };
  }, [loading]);

  return (
    <div className="grain relative min-h-screen bg-[#F3F0E9] text-[#0b0b0a]">
      {loading && (
        <Preloader
          lang={lang}
          done={() => {
            setLoading(false);
            setReady(true);
          }}
        />
      )}
      <Nav ready={ready} lang={lang} setLang={setLang} />
      <main>
        <Hero ready={ready} lang={lang} />
        <Interface lang={lang} />
        <Finale lang={lang} />
      </main>
    </div>
  );
}
