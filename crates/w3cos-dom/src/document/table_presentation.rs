//! HTML table presentational borders participate before author declarations.

use super::{Document,NodeId,NodeType};
use w3cos_std::style::Style;

pub(super) fn inherit_section_colors(tag: &str,style: &mut Style,parent: Option<&Style>) {
    if matches!(tag,"thead"|"tbody"|"tfoot"|"tr") && let Some(parent)=parent {
        style.border_color=parent.border_color;
        style.border_top_color=parent.border_top_color;
        style.border_right_color=parent.border_right_color;
        style.border_bottom_color=parent.border_bottom_color;
        style.border_left_color=parent.border_left_color;
        style.border_current_color=parent.border_current_color;
    }
}

fn border_width(document: &Document,id: NodeId) -> Option<u32> {
    let node=document.get_node(id);
    let value=node.attributes.iter().find(|(name,_)|name.as_str().eq_ignore_ascii_case("border"))?.1.as_str().trim_start();
    let digits:String=value.chars().take_while(char::is_ascii_digit).collect();
    Some(digits.parse::<u32>().unwrap_or(1))
}

pub(super) fn append_border_hints(document: &Document,id: NodeId,declarations: &mut Vec<(String,String,u32)>) {
    let node=document.get_node(id);
    if node.node_type!=NodeType::Element {return;}
    let tag=node.tag.as_str();
    if tag=="table" {
        if let Some(width)=border_width(document,id) {
            declarations.push(("border-width".into(),format!("{width}px"),0));
            declarations.push(("border-style".into(),"outset".into(),0));
        }
    } else if matches!(tag.as_str(),"td"|"th") {
        let mut parent=node.parent;
        while let Some(id)=parent {
            let ancestor=document.get_node(id);
            if ancestor.is_html_element && ancestor.tag.as_str()=="table" {
                if border_width(document,id).is_some_and(|width|width>0) {
                    declarations.push(("border-width".into(),"1px".into(),0));
                    declarations.push(("border-style".into(),"inset".into(),0));
                    declarations.push(("border-color".into(),"inherit".into(),0));
                }
                break;
            }
            parent=ancestor.parent;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::Document;
    use w3cos_std::{Color,style::{BorderLineStyle,BoxSizing}};

    #[test]
    fn html_empty_border_attribute_supplies_table_and_cell_frames() {
        for (attribute,width) in [("",1.0),("1",1.0),("3",3.0),("0",0.0)] {
            let mut document=Document::new();
            let table=document.create_element("table");
            table.set_attribute(&mut document,"border",attribute);
            let body=document.create_element("tbody");
            let row=document.create_element("tr");
            let cell=document.create_element("td");
            row.append_child(&mut document,cell);
            body.append_child(&mut document,row);
            table.append_child(&mut document,body);
            document.body().append_child(&mut document,table);
            let style=document.computed_style_for(table.id);
            assert_eq!(style.box_sizing,BoxSizing::BorderBox);
            assert_eq!(style.border_width,width,"border={attribute:?}");
            assert_eq!(style.border_color,Color::rgb(128,128,128));
            let cell=document.computed_style_for(cell.id);
            assert_eq!(cell.border_width,if width>0.0 {1.0}else{0.0});
            if width>0.0 {
                assert_eq!(style.border_styles,[Some(BorderLineStyle::Outset);4]);
                assert_eq!(cell.border_styles,[Some(BorderLineStyle::Inset);4]);
                assert_eq!(cell.border_color,Color::rgb(128,128,128));
            }
        }
    }

    #[test]
    fn html_border_hints_allow_author_overrides_and_inherit_table_color() {
        let mut document=Document::new();
        let table=document.create_element("table");
        table.set_attribute(&mut document,"border","");
        table.style_mut(&mut document).set_property("border-color","red");
        let row=document.create_element("tr");
        let cell=document.create_element("td");
        row.append_child(&mut document,cell);
        table.append_child(&mut document,row);
        document.body().append_child(&mut document,table);
        assert_eq!(document.computed_style_for(cell.id).border_color,Color::rgb(255,0,0));
        cell.style_mut(&mut document).set_property("border","2px solid blue");
        table.style_mut(&mut document).set_property("border","none");
        let table_style=document.computed_style_for(table.id);
        let cell_style=document.computed_style_for(cell.id);
        assert_eq!(table_style.border_width,0.0);
        assert_eq!(cell_style.border_width,2.0);
        assert_eq!(cell_style.border_styles,[Some(BorderLineStyle::Solid);4]);
        assert_eq!(cell_style.border_color,Color::rgb(0,0,255));
    }
}
