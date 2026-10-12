//! Column break planning keeps logical source coordinates separate from the
//! physical fragmentainer height. Ragged line ends and discarded margins must
//! be represented by real flow offsets, not height-based coordinate division.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ColumnFlowBand {
    pub start: f32,
    pub end: f32,
}

#[derive(Clone, Debug)]
pub(crate) struct ColumnFragmentPlan {
    pub height: f32,
    pub principal_height: f32,
    pub bands: Vec<ColumnFlowBand>,
    pub auto_height: bool,
    pub columns_per_set: usize,
    pub continues_in_parent: bool,
}

pub(crate) fn resolve(
    nodes: &[(&w3cos_std::style::Style, Option<usize>)],
    rects: &[Option<LayoutRect>],
    viewport: Option<(f32, f32)>,
) -> Vec<Option<ColumnFragmentPlan>> {
    use w3cos_std::style::ColumnFill;
    let mut contexts = vec![None;nodes.len()];
    let mut owners = vec![None;nodes.len()];
    let mut children = vec![false;nodes.len()];
    let mut excluded = vec![false;nodes.len()];
    let mut members = vec![Vec::new();nodes.len()];
    for (index,&(style,parent)) in nodes.iter().enumerate() {
        if let Some(parent) = parent { children[parent] = true; }
        excluded[index] = style.display == WDisplay::None
            || matches!(style.position,WPos::Absolute|WPos::Fixed)
            || parent.is_some_and(|parent| excluded[parent]);
        if excluded[index] { continue; }
        let Some(rect) = rects.get(index).copied().flatten() else { continue; };
        let (vw,vh) = viewport.unwrap_or((rect.width,rect.height));
        let padding = |spacing| resolve_spacing_for_layout(spacing,rect.width,style,vw,vh);
        let left = style.border_left_width.unwrap_or(style.border_width)+padding(style.padding.left);
        let right = style.border_right_width.unwrap_or(style.border_width)+padding(style.padding.right);
        let top = style.border_top_width.unwrap_or(style.border_width)+padding(style.padding.top);
        let bottom = style.border_bottom_width.unwrap_or(style.border_width)+padding(style.padding.bottom);
        let width = (rect.width-left-right).max(0.0);
        if let Some(column_width) = used_column_width(style,width,vw,vh) {
            let available = (!matches!(style.height,WDim::Auto)).then_some(rect.height-top-bottom);
            if available.is_none_or(|height| height.is_finite() && height>0.0) {
                let gap = style.column_gap.unwrap_or(style.font_size).max(0.0);
                let stride = column_width+gap;
                let count = if stride>0.0 { ((width+gap)/stride).round().max(1.0) as usize } else { 1 };
                contexts[index] = Some((rect.y+top,width,available,count,top+bottom));
            }
        }
        owners[index] = parent.and_then(|parent| {
            if contexts[parent].is_some() { Some(parent) } else { owners[parent] }
        });
        if let Some(owner) = owners[index] { members[owner].push(index); }
    }
    let mut plans: Vec<Option<ColumnFragmentPlan>> = vec![None;nodes.len()];
    // Nested principal heights must be known before their outer context is
    // balanced. Each node belongs to exactly one nearest flow owner.
    for owner in (0..nodes.len()).rev() {
        let Some((origin,width,available,count,_)) = contexts[owner] else { continue; };
        let mut end = origin;
        let mut protected = Vec::new();
        for &index in &members[owner] {
            let Some(rect) = rects[index] else { continue; };
            let style = nodes[index].0;
            let nested_height = plans[index].as_ref().filter(|plan| plan.auto_height)
                .map(|plan| plan.principal_height);
            let height = nested_height.unwrap_or(rect.height);
            let (vw,vh) = viewport.unwrap_or((width,height));
            let margin = resolve_spacing_for_layout(style.margin.bottom,width,style,vw,vh);
            end = end.max(rect.y+height+margin);
            if nested_height.is_some()
                || matches!(style.display,WDisplay::InlineBlock|WDisplay::InlineFlex|WDisplay::InlineTable)
            {
                if height>0.0 { protected.push(ColumnFlowBand { start:rect.y,end:rect.y+height }); }
            } else if !children[index] && style.display == WDisplay::Inline && height>0.0 {
                // Protect complete line boxes, including their leading. Ink
                // alone would permit a break at42 in three22px Times lines.
                let strut = inline_style_line_height(style).max(1.0/64.0);
                let font_box = inline_font_height(style);
                let full_strut = style.line_height_is_normal
                    && (height/strut-(height/strut).round()).abs()<0.0001;
                let leading = if full_strut {0.0} else {inline_upper_leading(strut,font_box)};
                let line_count = if full_strut {(height/strut).round().max(1.0) as usize}
                    else {((height-font_box).max(0.0)/strut).ceil() as usize+1};
                for line in 0..line_count {
                    let start = rect.y-leading+line as f32*strut;
                    protected.push(ColumnFlowBand { start,end:start+strut });
                    end = end.max(start+strut);
                }
            }
        }
        if !end.is_finite() || end<=origin { continue; }
        protected.sort_by(|a,b| a.start.total_cmp(&b.start).then(a.end.total_cmp(&b.end)));
        let mut ranges: Vec<ColumnFlowBand> = Vec::new();
        for range in protected {
            if let Some(last) = ranges.last_mut().filter(|last| range.start<last.end-0.01) {
                last.end = last.end.max(range.end);
            } else { ranges.push(range); }
        }
        if let Some(available) = available {
            // Preserve fixed-height sizing while retaining the flow-band
            // contract. Its existing compatibility renderer remains unchanged
            // until it consumes actual offsets rather than height alone.
            let height = if nodes[owner].0.column_fill == ColumnFill::Auto { available }
                else { (((end-origin)/count as f32*64.0).ceil()/64.0).max(1.0/64.0).min(available) };
            let bands = regular_bands(origin,end,height);
            plans[owner] = Some(ColumnFragmentPlan { height,principal_height:rects[owner].unwrap().height,
                bands,auto_height:false,columns_per_set:count,continues_in_parent:false });
            continue;
        }
        let count = if nodes[owner].0.column_fill == ColumnFill::Auto { 1 } else { count };
        let longest = ranges.iter().map(|range| range.end-range.start).fold(0.0_f32,f32::max);
        let lower = ((end-origin)/count as f32).max(longest);
        // Search LayoutUnits, not arbitrary float tolerances. A feasibility
        // pass packs complete ranges at legal breaks and truncates positive
        // gaps only when they become a new column's leading margin.
        let mut low = (lower*64.0).ceil().max(1.0) as u64;
        let mut high = ((end-origin).max(lower)*64.0).ceil().max(1.0) as u64;
        while low<high {
            let middle = low+(high-low)/2;
            if legal_bands(origin,end,middle as f32/64.0,&ranges,count).is_some() {
                high = middle;
            } else { low = middle+1; }
        }
        let height = low as f32/64.0;
        if let Some(bands) = legal_bands(origin,end,height,&ranges,count) {
            let edges = contexts[owner].unwrap().4;
            let principal_height = constrain_height(nodes[owner].0,height+edges,edges,
                rects[owner].unwrap(),viewport);
            plans[owner] = Some(ColumnFragmentPlan { height,principal_height,bands,auto_height:true,
                columns_per_set:count,continues_in_parent:false });
        }
    }
    resolve_nested_capacity(nodes,rects,viewport,&contexts,&owners,&members,&children,&mut plans);
    plans
}

type ColumnContext = (f32,f32,Option<f32>,usize,f32);

/// Inner column sets share the outer fragmentainer's block capacity. Once a
/// set is exhausted its next set resumes in the outer flow, not horizontally
/// beyond the inner container. Feasibility therefore has to be hierarchical.
fn resolve_nested_capacity(nodes:&[(&w3cos_std::Style,Option<usize>)],
    rects:&[Option<LayoutRect>],viewport:Option<(f32,f32)>,
    contexts:&[Option<ColumnContext>],owners:&[Option<usize>],members:&[Vec<usize>],
    children:&[bool],plans:&mut [Option<ColumnFragmentPlan>])
{
    let mut floors = vec![1.0_f32/64.0;nodes.len()];
    let mut nested = vec![false;nodes.len()];
    for owner in (0..nodes.len()).rev() {
        if contexts[owner].is_none() {continue;}
        for &index in &members[owner] {
            let style = nodes[index].0;
            if contexts[index].is_some_and(|context| context.2.is_none()) {
                nested[owner]=true;
                floors[owner]=floors[owner].max(floors[index]);
            } else if let Some(rect)=rects[index] {
                if !children[index] && style.display==WDisplay::Inline && rect.height>0.0 {
                    floors[owner]=floors[owner].max(inline_style_line_height(style));
                } else if matches!(style.display,WDisplay::InlineBlock|WDisplay::InlineFlex|WDisplay::InlineTable) {
                    floors[owner]=floors[owner].max(rect.height);
                }
            }
        }
    }
    for owner in 0..nodes.len() {
        let Some(context)=contexts[owner] else {continue;};
        // A containing automatic column context solves the whole subtree.
        if !nested[owner] || context.2.is_some()
            || owners[owner].is_some_and(|parent| contexts[parent].is_some_and(|c| c.2.is_none())) {continue;}
        let Some(initial)=plans[owner].as_ref() else {continue;};
        let mut low=(floors[owner]*64.0).ceil().max(1.0) as u64;
        let mut high=(initial.height.max(floors[owner])*64.0).ceil() as u64;
        while low<high {
            let mid=low+(high-low)/2;
            if pack_nested(owner,mid as f32/64.0,false,nodes,rects,viewport,
                contexts,members,children,plans).is_some() {high=mid;} else {low=mid+1;}
        }
        if let Some(resolved)=pack_nested(owner,low as f32/64.0,false,nodes,rects,viewport,
            contexts,members,children,plans) {
            for (index,plan) in resolved {plans[index]=Some(plan);}
        }
    }
}

fn pack_nested(owner:usize,height:f32,continuation:bool,
    nodes:&[(&w3cos_std::Style,Option<usize>)],rects:&[Option<LayoutRect>],
    viewport:Option<(f32,f32)>,contexts:&[Option<ColumnContext>],members:&[Vec<usize>],
    children:&[bool],existing:&[Option<ColumnFragmentPlan>])
    -> Option<Vec<(usize,ColumnFragmentPlan)>>
{
    let (origin,width,_,count,edges)=contexts[owner]?;
    let count=if nodes[owner].0.column_fill==w3cos_std::style::ColumnFill::Auto {1} else {count};
    let mut resolved=Vec::new();
    let mut ranges=Vec::new();
    let mut end=origin;
    for &index in &members[owner] {
        let Some(rect)=rects[index] else {continue;};
        let style=nodes[index].0;
        // A floating BFC begins before its child's retained margin. Keep that
        // start as a legal boundary, rather than discarding the whole margin
        // as ordinary inter-block space when the previous line fills a column.
        if style.float!=WFloat::None {
            ranges.push(ColumnFlowBand{start:rect.y,end:rect.y});
        }
        if contexts[index].is_some_and(|context| context.2.is_none()) {
            let child=pack_nested(index,height,true,nodes,rects,viewport,
                contexts,members,children,existing)?;
            let plan=&child.iter().find(|(id,_)| *id==index)?.1;
            let sets=plan.bands.len().div_ceil(plan.columns_per_set);
            for set in 0..sets {
                let start=rect.y+set as f32*height;
                ranges.push(ColumnFlowBand{start,end:start+height});
            }
            end=end.max(rect.y+plan.principal_height);
            resolved.extend(child);
            continue;
        }
        let block_height=existing[index].as_ref().map_or(rect.height,|plan| plan.principal_height);
        let (vw,vh)=viewport.unwrap_or((width,height));
        end=end.max(rect.y+block_height+resolve_spacing_for_layout(style.margin.bottom,width,style,vw,vh));
        if contexts[index].is_some()
            || matches!(style.display,WDisplay::InlineBlock|WDisplay::InlineFlex|WDisplay::InlineTable) {
            if block_height>0.0 {ranges.push(ColumnFlowBand{start:rect.y,end:rect.y+block_height});}
        } else if !children[index] && style.display==WDisplay::Inline && block_height>0.0 {
            let strut=inline_style_line_height(style).max(1.0/64.0);
            let font_box=inline_font_height(style);
            let full_strut=style.line_height_is_normal
                && (block_height/strut-(block_height/strut).round()).abs()<0.0001;
            let leading=if full_strut {0.0} else {inline_upper_leading(strut,font_box)};
            let lines=if full_strut {(block_height/strut).round().max(1.0) as usize}
                else {((block_height-font_box).max(0.0)/strut).ceil() as usize+1};
            for line in 0..lines {
                let start=rect.y-leading+line as f32*strut;
                ranges.push(ColumnFlowBand{start,end:start+strut});
                end=end.max(start+strut);
            }
        }
    }
    ranges.sort_by(|a,b| a.start.total_cmp(&b.start).then(a.end.total_cmp(&b.end)));
    let mut merged:Vec<ColumnFlowBand>=Vec::new();
    for range in ranges {
        if let Some(last)=merged.last_mut().filter(|last| range.start<last.end-0.01) {
            last.end=last.end.max(range.end);
        } else {merged.push(range);}
    }
    let bands=legal_bands(origin,end,height,&merged,if continuation {usize::MAX} else {count})?;
    let sets=bands.len().div_ceil(count).max(1);
    let principal_height=constrain_height(nodes[owner].0,height*sets as f32+edges,
        edges,rects[owner]?,viewport);
    resolved.push((owner,ColumnFragmentPlan{height,principal_height,bands,auto_height:true,
        columns_per_set:count,continues_in_parent:continuation}));
    Some(resolved)
}

fn constrain_height(style:&w3cos_std::Style,height:f32,edges:f32,rect:LayoutRect,
    viewport:Option<(f32,f32)>) -> f32
{
    let (vw,vh) = viewport.unwrap_or((rect.width,rect.height));
    let constraint = |dimension:WDim| dimension.resolve(rect.height,ROOT_FONT_SIZE,style.font_size,vw,vh)
        .map(|value| value+if style.box_sizing==WBoxSizing::ContentBox {edges} else {0.0});
    let height = constraint(style.max_height).map_or(height,|maximum| height.min(maximum));
    constraint(style.min_height).map_or(height,|minimum| height.max(minimum)).max(0.0)
}

/// Settle physical principal heights and subsequent normal flow while keeping
/// descendants in logical flow coordinates for the same paint/hit plan.
pub(crate) fn project(layouts:&mut [(LayoutRect,usize)],flat:&[FlatNodeInfo<'_>],vw:f32,vh:f32) {
    let count = flat.iter().filter(|node| establishes_column_context(node.style)
        && matches!(node.style.height,WDim::Auto)).count();
    if count==0 {return;}
    let mut positions = vec![None;flat.len()];
    for (position,&(_,index)) in layouts.iter().enumerate() {positions[index]=Some(position);}
    let nodes: Vec<_> = flat.iter().map(|node| (node.style,node.parent)).collect();
    let mut children = vec![Vec::new();flat.len()];
    let mut ends = vec![flat.len();flat.len()];
    let mut stack: Vec<usize> = Vec::new();
    for (index,node) in flat.iter().enumerate() {
        while stack.last().copied()!=node.parent {
            let Some(previous) = stack.pop() else {break;};
            ends[previous]=index;
        }
        if let Some(parent) = node.parent {children[parent].push(index);}
        stack.push(index);
    }
    // Compression inside an inner context can change a later sibling's source
    // offset in its outer context. Propagate outward to a fixed point, bounded
    // by the number of nested contexts rather than arbitrary retries.
    for _ in 0..=count {
        let old: Vec<_> = positions.iter().map(|position| position.map(|p| layouts[p].0)).collect();
        let plans = resolve(&nodes,&old,Some((vw,vh)));
        let mut changed = false;
        for index in (0..flat.len()).rev() {
            let Some(position) = positions[index] else {continue;};
            let style = flat[index].style;
            let normal = matches!(style.display,WDisplay::Block|WDisplay::FlowRoot|WDisplay::ListItem);
            let in_flow = |child:usize| flat[child].style.display!=WDisplay::None
                && flat[child].style.float==WFloat::None
                && !matches!(flat[child].style.position,WPos::Absolute|WPos::Fixed);
            if normal {
                let mut shift = 0.0_f32;
                let mut floor = f32::NEG_INFINITY;
                for &child in children[index].iter().filter(|child| in_flow(**child)) {
                    let (Some(prior),Some(child_position)) = (old[child],positions[child]) else {continue;};
                    if shift.abs()>0.01 && prior.y>=floor-0.01 {
                        for descendant in child..ends[child] {
                            if let Some(p) = positions[descendant] {layouts[p].0.y+=shift;}
                        }
                    }
                    let delta = layouts[child_position].0.height-prior.height;
                    if delta.abs()>0.01 {shift+=delta;floor=floor.max(prior.y+prior.height);}
                }
            }
            let target = if let Some(plan) = plans[index].as_ref().filter(|plan| plan.auto_height) {
                plan.principal_height
            } else if normal && matches!(style.height,WDim::Auto) {
                let bottom = |child:usize,rect:LayoutRect| rect.y+rect.height
                    + resolve_spacing_for_layout(flat[child].style.margin.bottom,
                        old[index].unwrap().width,flat[child].style,vw,vh);
                let old_end = children[index].iter().copied().filter(|child| in_flow(*child))
                    .filter_map(|child| old[child].map(|rect| bottom(child,rect))).reduce(f32::max);
                let new_end = children[index].iter().copied().filter(|child| in_flow(*child))
                    .filter_map(|child| positions[child].map(|p| bottom(child,layouts[p].0))).reduce(f32::max);
                if let (Some(old_end),Some(new_end)) = (old_end,new_end) {
                    constrain_height(style,old[index].unwrap().height+new_end-old_end,0.0,
                        old[index].unwrap(),Some((vw,vh)))
                } else {layouts[position].0.height}
            } else {layouts[position].0.height};
            if (target-layouts[position].0.height).abs()>0.01 {
                layouts[position].0.height=target;
                changed=true;
            }
        }
        if !changed {break;}
    }
}

fn regular_bands(origin:f32,end:f32,height:f32) -> Vec<ColumnFlowBand> {
    let count = ((end-origin)/height).ceil() as usize;
    (0..count).map(|column| ColumnFlowBand {
        start:origin+column as f32*height,
        end:(origin+(column+1) as f32*height).min(end),
    }).collect()
}

fn legal_bands(origin:f32,end:f32,height:f32,ranges:&[ColumnFlowBand],limit:usize)
    -> Option<Vec<ColumnFlowBand>>
{
    let mut bands = Vec::new();
    let mut start = origin;
    while start<end-0.01 {
        if bands.len()>=limit { return None; }
        let target = start+height;
        let cut = if target>=end-0.01 { end }
            else if let Some(range) = ranges.iter().find(|range|
                range.start<target-0.01 && range.end>target+0.01) {
                range.start
            } else {
                // The column ends in an inter-run gap. The next run's source
                // start is the real break offset, so the gap does not reappear
                // at the new column's top.
                ranges.iter().find(|range| range.start>=target-0.01)
                    .map_or(target,|range| range.start)
            };
        if cut<=start+0.01 { return None; }
        bands.push(ColumnFlowBand { start,end:cut });
        start = cut;
    }
    Some(bands)
}
