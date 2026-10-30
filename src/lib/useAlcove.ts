import { useEffect, useState } from "react";
import { APPEARANCE_EVENT } from "./appearance";

function readAlcove(): boolean {
  return typeof document !== "undefined" && document.documentElement.dataset.theme === "alcove";
}

/** True while the Alcove skin is applied (data-theme="alcove" on <html>, which
 *  also covers a custom theme whose base is alcove). For the few bits of
 *  markup Alcove needs that CSS cannot reach; everything else stays CSS. */
export function useAlcove(): boolean {
  const [on, setOn] = useState(readAlcove);
  useEffect(() => {
    const sync = () => setOn(readAlcove());
    window.addEventListener(APPEARANCE_EVENT, sync);
    // A live studio preview can flip data-theme without the event; watch the
    // attribute too so the markup never lags the stylesheet.
    const mo = new MutationObserver(sync);
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    sync();
    return () => {
      window.removeEventListener(APPEARANCE_EVENT, sync);
      mo.disconnect();
    };
  }, []);
  return on;
}
