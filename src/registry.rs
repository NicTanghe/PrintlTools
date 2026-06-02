#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolId {
    FolderPageCounter,
    UsbSafeEject,
    PdfJoiner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ToolStatus {
    Ready,
    Planned,
}

#[derive(Debug, Clone, Copy)]
pub struct ToolDefinition {
    pub id: ToolId,
    pub name: &'static str,
    pub short_name: &'static str,
    pub description: &'static str,
    pub status: ToolStatus,
}

pub const TOOLS: &[ToolDefinition] = &[
    ToolDefinition {
        id: ToolId::FolderPageCounter,
        name: "Folder page counter",
        short_name: "Page counter",
        description: "Select a folder and prepare a page-count run.",
        status: ToolStatus::Ready,
    },
    ToolDefinition {
        id: ToolId::UsbSafeEject,
        name: "USB safe eject",
        short_name: "USB eject",
        description: "Select a removable drive and prepare safe eject.",
        status: ToolStatus::Ready,
    },
    ToolDefinition {
        id: ToolId::PdfJoiner,
        name: "PDF joiner",
        short_name: "PDF joiner",
        description: "Select PDF files and prepare a merged output.",
        status: ToolStatus::Ready,
    },
];

pub fn all_tools() -> &'static [ToolDefinition] {
    TOOLS
}
