"""Export UperNet (ADE20K, 150 scene classes) to the ONNX file the Sky mask uses.

    pip install torch transformers onnx onnxruntime
    python tools/export_upernet.py . openmmlab/upernet-swin-large \
        "$HOME/Library/Application Support/io.github.CyberTimon.RapidRAW/models/upernet_swin_large.onnx"

The model is exported at a fixed 768x768 input (its pyramid pooling cannot
be traced with a dynamic size); the app letterboxes photos into that and
also runs overlapping tiles at 1536 px for thin structures. The script
checks the ONNX output against PyTorch before writing the label list.
"""
import sys, time, torch, numpy as np
from transformers import UperNetForSemanticSegmentation
S=sys.argv[1]; name=sys.argv[2]; out=sys.argv[3]
m=UperNetForSemanticSegmentation.from_pretrained(name).eval()
labels=m.config.id2label
print("classes", len(labels), {k:labels[k] for k in list(labels)[:6]})
class Wrap(torch.nn.Module):
    def __init__(s,m): super().__init__(); s.m=m
    def forward(s,x): return torch.softmax(s.m(pixel_values=x).logits, dim=1)
w=Wrap(m).eval()
x=torch.randn(1,3,768,768)
with torch.no_grad():
    t=time.time(); ref=w(x); print("torch out", tuple(ref.shape), "%.2fs"%(time.time()-t))
torch.onnx.export(w, (x,), out, input_names=["pixel_values"], output_names=["probs"],
    opset_version=17, dynamo=False)
import onnxruntime as ort
s=ort.InferenceSession(out, providers=["CPUExecutionProvider"])
for shape in ((1,3,768,768),):
    xi=torch.randn(*shape)
    with torch.no_grad(): r=w(xi).numpy()
    t=time.time(); o=s.run(None,{"pixel_values":xi.numpy()})[0]
    print(shape, "ort %.2fs"%(time.time()-t), "max abs diff %.2e"%np.abs(o-r).max())
import json; json.dump({int(k):v for k,v in labels.items()}, open(out+".labels.json","w"))
