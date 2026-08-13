#!/usr/bin/env python3
"""Generates tests/fixtures/dummy_identity.onnx — a minimal ONNX model whose
graph is a single Identity node with a dynamically-shaped float input.

Used by the model-registry round-trip test. Hand-encodes the protobuf wire
format so no onnx/protobuf package is required. Regenerate with:
    python3 scripts/make_dummy_model.py
"""

import os


def varint(n: int) -> bytes:
    out = b""
    while True:
        b7 = n & 0x7F
        n >>= 7
        if n:
            out += bytes([b7 | 0x80])
        else:
            return out + bytes([b7])


def tag(field: int, wire_type: int) -> bytes:
    return varint((field << 3) | wire_type)


def ld(field: int, payload: bytes) -> bytes:
    """Length-delimited field (strings, sub-messages)."""
    return tag(field, 2) + varint(len(payload)) + payload


def s(field: int, string: str) -> bytes:
    return ld(field, string.encode())


def i(field: int, n: int) -> bytes:
    return tag(field, 0) + varint(n)


# TensorShapeProto: repeated Dimension dim = 1; Dimension.dim_param = 2
shape = b"".join(ld(1, s(2, name)) for name in ("batch", "channels", "height", "width"))

# TypeProto.Tensor: elem_type = 1 (1 = FLOAT), shape = 2
tensor_type = i(1, 1) + ld(2, shape)

# TypeProto: tensor_type = 1
type_proto = ld(1, tensor_type)


def value_info(name: str) -> bytes:
    # ValueInfoProto: name = 1, type = 2
    return s(1, name) + ld(2, type_proto)


# NodeProto: input = 1, output = 2, name = 3, op_type = 4
node = s(1, "input") + s(2, "output") + s(3, "identity_node") + s(4, "Identity")

# GraphProto: node = 1, name = 2, input = 11, output = 12
graph = ld(1, node) + s(2, "dummy_identity") + ld(11, value_info("input")) + ld(12, value_info("output"))

# OperatorSetIdProto: domain = 1 (default ai.onnx), version = 2
opset = s(1, "") + i(2, 13)

# ModelProto: ir_version = 1, producer_name = 2, graph = 7, opset_import = 8
model = i(1, 8) + s(2, "rapidraw-dummy") + ld(7, graph) + ld(8, opset)

out_path = os.path.join(os.path.dirname(__file__), "..", "tests", "fixtures", "dummy_identity.onnx")
os.makedirs(os.path.dirname(out_path), exist_ok=True)
with open(out_path, "wb") as f:
    f.write(model)
print(f"Wrote {os.path.normpath(out_path)} ({len(model)} bytes)")
