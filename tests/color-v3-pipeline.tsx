// Browser-only state fixture: never reads/writes photos, assets or sidecars.
import React, { useState } from 'react';
import { createRoot } from 'react-dom/client';
import i18next from 'i18next';
import { initReactI18next } from 'react-i18next';
import { ColorV3Switch } from '../src/components/adjustments/ColorV3';
import { useEditorStore } from '../src/store/useEditorStore';
import { THEMES } from '../src/utils/themes';
import '../src/styles.css';

await i18next.use(initReactI18next).init({ lng:'en', resources:{en:{translation:{}}} });
let fail = false;
Object.assign(window, { __TAURI_INTERNALS__: { invoke: async (command: string) => {
  if (command !== 'pin_color_v3') throw new Error('Unexpected fixture command');
  await new Promise(resolve => setTimeout(resolve, 250));
  if (fail) throw new Error('Pinned transform is missing. Restore the asset from your backup.');
  return { pipeline:{ schema:1, engine:'v3-stable-input-2',input_policy:'profiled-display-cube-or-wide-gamut-bypass-1',
    raw_development:'bayer-d65-green-clipped-neutral-1',input_transform:null,output_transform:null } };
} } });
useEditorStore.getState().setEditor({ selectedImage: { path:'synthetic-photo' } as any });
function Fixture() {
  const [edits,setEdits] = useState<any>({processVersion:3,v3:{}});
  return <main style={{maxWidth:340,padding:16}} className="text-text-primary">
    <h1>Pipeline identity verification</h1>
    <button onClick={() => {setEdits({processVersion:3,v3:{}});fail=false;}}>Reset older edit</button>
    <button onClick={() => {fail=true;setEdits({processVersion:3,v3:{}});}}>Simulate failure</button>
    <ColorV3Switch adjustments={edits} setAdjustments={setEdits}/>
    <output aria-label="Saved identity" style={{overflowWrap:'anywhere'}}>{JSON.stringify(edits.v3Pipeline ?? null)}</output>
    <output aria-label="RAW recovery">{edits.v3RawRecovery ?? 'neutral_green_v1'}</output>
  </main>;
}
Object.entries({...THEMES[0].cssVariables,'--font-family':'system-ui'}).forEach(([k,v]) => document.documentElement.style.setProperty(k,String(v)));
document.body.style.background='var(--app-bg-primary)';
createRoot(document.getElementById('root')!).render(<Fixture/>);
