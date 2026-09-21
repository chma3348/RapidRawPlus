// Standalone manual QA fixture. Does not load photographs, sidecars or Tauri.
import React, { useState } from 'react';
import { createRoot } from 'react-dom/client';
import i18next from 'i18next';
import { initReactI18next } from 'react-i18next';
import ColorV3Advanced from '../src/components/adjustments/ColorV3Advanced';
import { defaultV3Controls, defaultV3Range } from '../src/utils/colorV3';
import { THEMES } from '../src/utils/themes';
import '../src/styles.css';

await i18next.use(initReactI18next).init({ lng: 'en', fallbackLng: 'en', resources: { en: { translation: {} } } });
// Test-only IPC double: browser UI states without opening a user photograph.
let failInspection = false;
const canvas = document.createElement('canvas');
canvas.width = 256;
canvas.height = 128;
const ctx = canvas.getContext('2d')!;
const gradient = ctx.createLinearGradient(0, 0, 256, 0);
gradient.addColorStop(0, '#b4533e');
gradient.addColorStop(1, '#368ba6');
ctx.fillStyle = gradient;
ctx.fillRect(0, 0, 256, 128);
const sampleImage = canvas.toDataURL();
const gray = ctx.createLinearGradient(0, 0, 256, 0);
gray.addColorStop(0, 'black');
gray.addColorStop(1, 'white');
ctx.fillStyle = gray;
ctx.fillRect(0, 0, 256, 128);
const sampleMatte = canvas.toDataURL();
Object.assign(window, {
  __TAURI_INTERNALS__: {
    invoke: async (command: string, args: any) => {
      if (command !== 'inspect_color_v3') throw new Error('Unsupported fixture command');
      await new Promise((resolve) => setTimeout(resolve, 150));
      if (failInspection) throw new Error('Simulated inspection failure');
      return {
        image: sampleImage,
        selection: sampleMatte,
        center: args.point ? [args.point[0] * 360, 0.12, 0.65] : null,
      };
    },
  },
});
function Fixture() {
  const [values, setValues] = useState(() => ({ ...defaultV3Controls(), ranges: [defaultV3Range()] }));
  return (
    <main style={{ width: 'min(100%, 340px)', padding: 16, boxSizing: 'border-box' }}>
      <h1 className="text-lg text-text-primary">V3 control verification</h1>
      <button
        type="button"
        className="text-text-primary"
        onClick={() => {
          failInspection = !failInspection;
          setValues((prev) => ({ ...prev, exposure: prev.exposure === 0 ? 0.1 : 0 }));
        }}
      >
        Toggle simulated inspection failure
      </button>
      <ColorV3Advanced
        values={values}
        inspection={{ path: 'synthetic-fixture', edits: { processVersion: 3, v3: values } }}
        update={(key, value) => setValues((prev) => ({ ...prev, [key]: value }))}
      />
      <output className="block text-sm text-text-primary mt-4" aria-label="Stored curve">
        {JSON.stringify(values.curve)}
      </output>
      <output className="block text-sm text-text-primary" aria-label="Stored range center">
        {JSON.stringify(values.ranges[0]?.center)}
      </output>
    </main>
  );
}
Object.entries({ ...THEMES[0].cssVariables, '--font-family': 'system-ui' }).forEach(([k, v]) =>
  document.documentElement.style.setProperty(k, String(v)),
);
document.body.style.background = 'var(--app-bg-primary)';
createRoot(document.getElementById('root')!).render(<Fixture />);
