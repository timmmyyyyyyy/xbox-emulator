#version 450
layout(set = 0, binding = 0) readonly buffer B { uvec4 v[]; } b;
layout(push_constant) uniform P { uint base; } p;
layout(location = 0) out vec4 o;
void main() { o = uintBitsToFloat(b.v[p.base + uint(gl_FragCoord.x)]); }
