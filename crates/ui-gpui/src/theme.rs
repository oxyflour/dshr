//! DSH Desktop 设计 token（从安装包的 design system 原样解析）。
//!
//! 来源（本机 DSH 桌面版 `0.2.2-rc.1` 的 `@deepseek-ai/dsh-client-ui-theme`）：
//! `--dsw-static-*` 调色板 + `--dsw-alias-*` 语义别名 + `body[data-ds-dark-theme]` 覆盖。
//! 这里的数值是从 CSS 里**解析后**的结果（var 链已展开），不是目测配色。
//!
//! 只收录界面真正用到的 token；新增组件时按需补充，不要凭感觉造颜色。

use gpui::{px, rgba, Pixels, Rgba};

/// 主题选择来自 DSH 的 `ui-theme.preference`（light / dark）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeKind {
    #[default]
    Light,
    Dark,
}

impl ThemeKind {
    pub fn toggled(self) -> Self {
        match self {
            Self::Light => Self::Dark,
            Self::Dark => Self::Light,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Light => "浅色",
            Self::Dark => "深色",
        }
    }
}

/// 语义色板；每个值都是 `0xRRGGBBAA`，转 `Rgba` 见 [`Palette::color`]。
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub bg_base: u32,
    pub bg_layer_1: u32,
    pub bg_layer_2: u32,
    pub bg_layer_3: u32,
    pub sidebar_fill: u32,
    pub label_primary: u32,
    pub label_secondary: u32,
    pub label_tertiary: u32,
    pub label_caption: u32,
    pub border_l1: u32,
    pub border_l2: u32,
    pub border_l3: u32,
    pub border_l4: u32,
    pub bubble: u32,
    pub bubble_highlight: u32,
    pub input_major: u32,
    pub menu: u32,
    pub button_info: u32,
    pub button_info_hover: u32,
    pub button_elevated: u32,
    pub interactive_hover: u32,
    pub interactive_hover_solid: u32,
    pub code_block: u32,
    pub code_block_banner: u32,
    pub inline_code: u32,
    pub tag: u32,
    pub link: u32,
    pub state_error: u32,
    pub state_success: u32,
    pub state_warn: u32,
    pub business_primary: u32,
    pub business_tertiary: u32,
    pub scrollbar: u32,
    pub scrollbar_hover: u32,
}

impl Palette {
    /// 浅色（`body` 缺省值）。
    pub const LIGHT: Self = Self {
        bg_base: 0xffff_ffff,
        bg_layer_1: 0xffff_ffff,
        bg_layer_2: 0xffff_ffff,
        bg_layer_3: 0xffff_ffff,
        sidebar_fill: 0xf9fa_fbff,
        label_primary: 0x0f11_15ff,
        label_secondary: 0x6166_6bff,
        label_tertiary: 0x8185_8cff,
        label_caption: 0xadb2_b8ff,
        border_l1: 0x0000_000a,
        border_l2: 0x0000_001a,
        border_l3: 0x0000_001f,
        border_l4: 0x0000_0029,
        bubble: 0xedf3_feff,
        bubble_highlight: 0xd3e2_ffff,
        input_major: 0xffff_ffff,
        menu: 0xfcfd_feb3,
        button_info: 0x4176_e6ff,
        button_info_hover: 0x7aaa_ffff,
        button_elevated: 0xffff_ffff,
        interactive_hover: 0x2631_480f,
        interactive_hover_solid: 0xf1f3_f5ff,
        code_block: 0xf9fa_fbff,
        code_block_banner: 0xf9fa_fbff,
        inline_code: 0xfafa_faff,
        tag: 0xf1f3_f5ff,
        link: 0x4176_e6ff,
        state_error: 0xec13_13ff,
        state_success: 0x22c5_5eff,
        state_warn: 0xf59e_0bff,
        business_primary: 0x4176_e6ff,
        business_tertiary: 0xe4ed_fdff,
        scrollbar: 0xe5e5_e5ff,
        scrollbar_hover: 0xd4d4_d4ff,
    };

    /// 深色（`body[data-ds-dark-theme]` 覆盖）。
    pub const DARK: Self = Self {
        bg_base: 0x1515_17ff,
        bg_layer_1: 0x2323_24ff,
        bg_layer_2: 0x2c2c_2eff,
        bg_layer_3: 0x3536_38ff,
        sidebar_fill: 0x1b1b_1cff,
        label_primary: 0xf9fa_fbff,
        label_secondary: 0xcfd3_d6ff,
        label_tertiary: 0xadb2_b8ff,
        label_caption: 0x8185_8cff,
        border_l1: 0xffff_ff0f,
        border_l2: 0xffff_ff1f,
        border_l3: 0xffff_ff29,
        border_l4: 0xffff_ff33,
        bubble: 0x2c2c_2eff,
        bubble_highlight: 0x4345_4aff,
        input_major: 0x2c2c_2eff,
        menu: 0x3031_36f0,
        button_info: 0x7aaa_ffff,
        button_info_hover: 0x4176_e6ff,
        button_elevated: 0x4345_4aff,
        interactive_hover: 0xffff_ff14,
        interactive_hover_solid: 0x3536_38ff,
        code_block: 0x1b1b_1cff,
        code_block_banner: 0x2c2c_2eff,
        inline_code: 0x2929_29ff,
        tag: 0x2c2c_2eff,
        link: 0x7aaa_ffff,
        state_error: 0xf25a_5aff,
        state_success: 0x22c5_5eff,
        state_warn: 0xf59e_0bff,
        business_primary: 0x7aaa_ffff,
        business_tertiary: 0x3441_5bff,
        scrollbar: 0x3c3c_3dff,
        scrollbar_hover: 0x6567_6bff,
    };

    pub fn color(self, token: u32) -> Rgba {
        rgba(token)
    }
}

/// 圆角 token：`--dsw-radius-*`。
#[derive(Debug, Clone, Copy)]
pub struct Radii {
    pub xs: Pixels,
    pub sm: Pixels,
    pub md: Pixels,
    pub lg: Pixels,
    pub xl: Pixels,
    pub panel: Pixels,
}

impl Radii {
    pub const DSH: Self = Self {
        xs: px(4.0),
        sm: px(8.0),
        md: px(12.0),
        lg: px(16.0),
        xl: px(20.0),
        panel: px(28.0),
    };
}

/// 排版：正文 14/24，secondary 13/20，small 12/18（DSH `--dsw-font-*` / `--dsh-content-font-*`）。
#[derive(Debug, Clone, Copy)]
pub struct Typography {
    pub body: Pixels,
    pub body_line: Pixels,
    pub secondary: Pixels,
    pub secondary_line: Pixels,
    pub small: Pixels,
    pub small_line: Pixels,
    pub h1: Pixels,
    pub h1_line: Pixels,
    pub h2: Pixels,
    pub h2_line: Pixels,
    pub h3: Pixels,
    pub h3_line: Pixels,
}

impl Typography {
    pub const DSH: Self = Self {
        body: px(14.0),
        body_line: px(24.0),
        secondary: px(13.0),
        secondary_line: px(20.0),
        small: px(12.0),
        small_line: px(18.0),
        h1: px(21.0),
        h1_line: px(30.0),
        h2: px(19.0),
        h2_line: px(28.0),
        h3: px(18.0),
        h3_line: px(26.0),
    };
}

/// 布局常量：来自 chat / layout 包的 CSS 变量。
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    /// DSH 缺省侧栏宽度（`layoutInfo.sidebar || 280`）。
    pub sidebar_width: Pixels,
    /// `--dsh-chat-content-width` 的缺省回退值（CSS 里是 clamp(680, …×.64, 920)）。
    pub content_width: Pixels,
    /// `--dsh-composer-card-max-width` = content + 32。
    pub composer_max_width: Pixels,
    /// `--dsh-composer-side-clearance`。
    pub composer_clearance: Pixels,
    /// 头部（带 tabs 时 min-height:76px，无 tabs 时紧凑）。
    pub header_height: Pixels,
    /// 会话滚动区内边距（`padding:16px calc(side-clearance + 16px)`）。
    pub page_padding: Pixels,
}

impl Layout {
    pub const DSH: Self = Self {
        sidebar_width: px(280.0),
        content_width: px(748.0),
        composer_max_width: px(780.0),
        composer_clearance: px(16.0),
        header_height: px(76.0),
        page_padding: px(16.0),
    };
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub kind: ThemeKind,
    pub palette: Palette,
    pub radii: Radii,
    pub type_scale: Typography,
    pub layout: Layout,
    /// 代码字体（DSH `--dsh-font-family-code-default` 的 Windows 首选）。
    pub font_code: &'static str,
}

impl Theme {
    pub fn new(kind: ThemeKind) -> Self {
        Self {
            kind,
            palette: match kind {
                ThemeKind::Light => Palette::LIGHT,
                ThemeKind::Dark => Palette::DARK,
            },
            radii: Radii::DSH,
            type_scale: Typography::DSH,
            layout: Layout::DSH,
            font_code: "Consolas",
        }
    }

    pub fn light() -> Self {
        Self::new(ThemeKind::Light)
    }

    pub fn dark() -> Self {
        Self::new(ThemeKind::Dark)
    }

    pub fn toggle(self) -> Self {
        Self::new(self.kind.toggled())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palettes_match_the_dsh_tokens() {
        // 值来自 `--dsw-alias-*` 解析结果（见 docs/ui-dsh-alignment.md）。
        assert_eq!(Palette::LIGHT.bg_base, 0xffff_ffff);
        assert_eq!(Palette::LIGHT.sidebar_fill, 0xf9fa_fbff);
        assert_eq!(Palette::LIGHT.bubble, 0xedf3_feff);
        assert_eq!(Palette::DARK.bg_base, 0x1515_17ff);
        assert_eq!(Palette::DARK.sidebar_fill, 0x1b1b_1cff);
        assert_eq!(Palette::DARK.label_primary, 0xf9fa_fbff);
        // 边框在深色下是白色低透明度。
        assert_eq!(Palette::DARK.border_l2, 0xffff_ff1f);
    }

    #[test]
    fn radii_and_layout_match_dsh_css() {
        let theme = Theme::light();
        assert_eq!(theme.radii.panel, px(28.0));
        assert_eq!(theme.radii.lg, px(16.0));
        assert_eq!(theme.layout.sidebar_width, px(280.0));
        assert_eq!(theme.layout.content_width, px(748.0));
        assert_eq!(theme.layout.composer_max_width, px(780.0));
        assert_eq!(theme.layout.header_height, px(76.0));
    }

    #[test]
    fn toggle_switches_palette() {
        let light = Theme::light();
        let dark = light.toggle();
        assert_eq!(dark.kind, ThemeKind::Dark);
        assert_eq!(dark.palette.bg_base, Palette::DARK.bg_base);
        assert_eq!(dark.toggle().palette.bg_base, Palette::LIGHT.bg_base);
    }
}
