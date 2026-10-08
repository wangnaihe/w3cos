//! Axis-aligned circular-corner fill/stroke coverage using Graphite's analytic
//! edge equations and device-snapped corner mesh. Unsupported draws fall back.
use skia_safe::{Canvas, Color, Data, Paint, Rect, RuntimeEffect, paint};

// Share the same 36-vertex interpolation topology as analytic ovals. The
// fragment tail differs: strokes evaluate both sides of a circular edge.
fn shader_source() -> String {
    let prefix = super::analytic_ellipse::OVAL_SHADER.split_once(" float2 r=").unwrap().0;
    let mut source = prefix.replace("uniform float4 bounds; uniform float4 rgba;",
        "uniform float4 bounds; uniform float4 rgba; uniform float4 geometry;");
    source.push_str(r#"
 if(!hit)return half4(0);
 float2 edge=min(distances.xy,distances.zw),r=geometry.xy,uv=r-edge;
 float s=geometry.z,d=min(edge.x,edge.y)+s;
 float inner=min(edge.x,edge.y)-s;
 if(all(greaterThan(uv,float2(0)))){
  float2 invR2=1/(r*r+s*s),normUV=invR2*uv;
  float invGrad=inversesqrt(max(dot(normUV,normUV),1.1755e-38));
  float f=.5*invGrad*(dot(uv,normUV)-1),width=r.x*s*invR2.x*invGrad;
  d=min(d,width-f);
  inner=min(inner,-width-f);
 }
 float coverage=clamp(.5+(s>0?min(d-outset,-inner):d-outset),0,1);
 return half4(rgba*coverage);
}"#);
    source
}

thread_local! {
    static SHADER: Option<RuntimeEffect> = RuntimeEffect::make_for_shader(shader_source(), None).ok();
}

fn uniforms(bounds: Rect, radius: f32, stroke: f32, color: skia_safe::Color4f) -> Vec<f32> {
    let mut values = vec![bounds.left,bounds.top,bounds.right,bounds.bottom,
        color.r*color.a,color.g*color.a,color.b*color.a,color.a,radius,radius,stroke,0.0];
    let mut vertices = Vec::new();
    let mut edges = Vec::new();
    let k = 0.41421356237_f32;
    for corner in 0..4 {
        let sx = if corner==0 || corner==3 { -1.0 } else { 1.0 };
        let sy = if corner<2 { -1.0 } else { 1.0 };
        let cx = if sx<0.0 { bounds.left+radius } else { bounds.right-radius };
        let cy = if sy<0.0 { bounds.top+radius } else { bounds.bottom-radius };
        let outer = radius+stroke;
        let mut anchors = [[outer,k*outer],[k*outer,outer]];
        let inset = radius-stroke-1.0;
        let mut inner = [[inset,0.0],[0.0,inset]];
        let mut axes = [[1.0,0.0],[0.0,1.0]];
        if corner%2!=0 { anchors.swap(0,1);inner.swap(0,1);axes.swap(0,1); }
        for id in 0..9 {
            let uv = if id<6 { anchors[if matches!(id,2|3|5) {1} else {0}] }
                else if id==8 && stroke==0.0 { [0.0,0.0] }
                else { inner[if id==7 {1} else {0}] };
            let mut px=cx+sx*uv[0];
            let mut py=cy+sy*uv[1];
            if id==8 && stroke==0.0 {
                px=(bounds.left+bounds.right)*0.5;py=(bounds.top+bounds.bottom)*0.5;
            }
            let normal = if id==0 { axes[0] } else if id==3 { axes[1] }
                else { [std::f32::consts::FRAC_1_SQRT_2;2] };
            let (dx,dy)=if id<4 { (px+sx*normal[0],py+sy*normal[1]) } else { (px,py) };
            vertices.extend_from_slice(&[(dx*256.0).round()/256.0,(dy*256.0).round()/256.0,
                if id<4 {1.0} else {0.0},0.0]);
            edges.extend_from_slice(&[px-bounds.left,py-bounds.top,bounds.right-px,bounds.bottom-py]);
        }
    }
    values.extend(vertices);values.extend(edges);values
}

pub(super) fn draw(canvas: &Canvas, bounds: Rect, radius: f32, paint: &Paint) -> bool {
    let stroke = match paint.style() { paint::Style::Fill=>0.0,
        paint::Style::Stroke=>paint.stroke_width()*0.5,_=>return false };
    if !canvas.local_to_device_as_3x3().is_identity() || !bounds.is_finite()
        || radius<=stroke+1.0 || bounds.width()<=2.0*(radius+stroke+1.0)
        || bounds.height()<=2.0*(radius+stroke+1.0) || paint.shader().is_some()
        || paint.mask_filter().is_some() || paint.image_filter().is_some()
        || paint.color_filter().is_some() || paint.path_effect().is_some()
    { return false; }
    SHADER.with(|effect| {
        let Some(effect)=effect else {return false};
        let bytes:Vec<u8>=uniforms(bounds,radius,stroke,paint.color4f()).iter()
            .flat_map(|value|value.to_ne_bytes()).collect();
        let Some(shader)=effect.make_shader(Data::new_copy(&bytes),&[],None) else {return false};
        let mut analytic=paint.clone();
        analytic.set_style(paint::Style::Fill).set_color(Color::WHITE).set_shader(shader).set_anti_alias(false);
        let extent=stroke+1.0;
        canvas.draw_rect(Rect::new((bounds.left-extent).floor(),(bounds.top-extent).floor(),
            (bounds.right+extent).ceil(),(bounds.bottom+extent).ceil()),&analytic);
        true
    })
}
