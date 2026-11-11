use ratatui::style::Color;
use serde::Deserialize;

/// Only these built-in palettes are accepted in configuration.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    #[default]
    Default,
    TokyoNight,
    Dracula,
    Nord,
    Gruvbox,
    CatppuccinMocha,
    OneDark,
    SolarizedDark,
    SolarizedLight,
    GithubLight,
}

#[derive(Debug)]
pub struct Palette {
    pub bg: Color,
    pub panel: Color,
    pub text: Color,
    pub muted: Color,
    pub border: Color,
    pub accent: Color,
    pub green: Color,
    pub red: Color,
    pub add_bg: Color,
    pub del_bg: Color,
    pub hunk_bg: Color,
    pub add_word_bg: Color,
    pub del_word_bg: Color,
    pub syntax: SyntaxColors,
}

#[derive(Debug)]
pub struct SyntaxColors {
    pub attribute: Color,
    pub number: Color,
    pub comment: Color,
    pub type_color: Color,
    pub keyword: Color,
    pub function: Color,
    pub label: Color,
    pub operator: Color,
    pub punctuation: Color,
    pub string: Color,
    pub tag: Color,
}

impl Palette {
    /// Colors in the same order as highlight::CAPTURES.
    pub fn syntax_colors(&self) -> [Color; 19] {
        let s = &self.syntax;
        [
            s.attribute,
            s.number,
            s.comment,
            s.number,
            s.type_color,
            self.text,
            s.keyword,
            s.function,
            s.keyword,
            s.label,
            s.number,
            s.operator,
            s.type_color,
            s.punctuation,
            s.string,
            s.tag,
            s.type_color,
            self.text,
            s.tag,
        ]
    }
}

impl Theme {
    pub const fn palette(self) -> &'static Palette {
        match self {
            Self::Default => &DEFAULT,
            Self::TokyoNight => &TOKYO_NIGHT,
            Self::Dracula => &DRACULA,
            Self::Nord => &NORD,
            Self::Gruvbox => &GRUVBOX,
            Self::CatppuccinMocha => &CATPPUCCIN_MOCHA,
            Self::OneDark => &ONE_DARK,
            Self::SolarizedDark => &SOLARIZED_DARK,
            Self::SolarizedLight => &SOLARIZED_LIGHT,
            Self::GithubLight => &GITHUB_LIGHT,
        }
    }

    pub fn name(self) -> &'static str {
        Self::ALL[self.index()].0
    }

    pub fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()].1
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|&(_, theme)| theme == self)
            .expect("Every built-in theme is listed")
    }

    pub const ALL: [(&'static str, Self); 10] = [
        ("default", Self::Default),
        ("tokyo-night", Self::TokyoNight),
        ("dracula", Self::Dracula),
        ("nord", Self::Nord),
        ("gruvbox", Self::Gruvbox),
        ("catppuccin-mocha", Self::CatppuccinMocha),
        ("one-dark", Self::OneDark),
        ("solarized-dark", Self::SolarizedDark),
        ("solarized-light", Self::SolarizedLight),
        ("github-light", Self::GithubLight),
    ];
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

const DEFAULT: Palette = Palette {
    bg: rgb(0x000000),
    panel: rgb(0x0a0a0a),
    text: rgb(0xd4d4d4),
    muted: rgb(0x808080),
    border: rgb(0x303030),
    accent: rgb(0x7aa2f7),
    green: rgb(0x9ece6a),
    red: rgb(0xf7768e),
    add_bg: rgb(0x081c0e),
    del_bg: rgb(0x200a0c),
    hunk_bg: rgb(0x181818),
    add_word_bg: rgb(0x184620),
    del_word_bg: rgb(0x54181e),
    syntax: SyntaxColors {
        attribute: rgb(0xe5c07b),
        number: rgb(0xff9e64),
        comment: rgb(0x73829b),
        type_color: rgb(0x7dcfff),
        keyword: rgb(0xbb9af7),
        function: rgb(0x7aa2f7),
        label: rgb(0xe0af68),
        operator: rgb(0x89ddff),
        punctuation: rgb(0xa9b1d6),
        string: rgb(0x9ece6a),
        tag: rgb(0xf7768e),
    },
};

const TOKYO_NIGHT: Palette = Palette {
    bg: rgb(0x1a1b26),
    panel: rgb(0x16161e),
    text: rgb(0xc0caf5),
    muted: rgb(0x737aa2),
    border: rgb(0x414868),
    accent: rgb(0x7aa2f7),
    green: rgb(0x9ece6a),
    red: rgb(0xf7768e),
    add_bg: rgb(0x20332e),
    del_bg: rgb(0x3c242f),
    hunk_bg: rgb(0x292e42),
    add_word_bg: rgb(0x344c38),
    del_word_bg: rgb(0x663349),
    syntax: SyntaxColors {
        attribute: rgb(0xe0af68),
        number: rgb(0xff9e64),
        comment: rgb(0x565f89),
        type_color: rgb(0x7dcfff),
        keyword: rgb(0xbb9af7),
        function: rgb(0x7aa2f7),
        label: rgb(0xe0af68),
        operator: rgb(0x89ddff),
        punctuation: rgb(0x9aa5ce),
        string: rgb(0x9ece6a),
        tag: rgb(0xf7768e),
    },
};

const DRACULA: Palette = Palette {
    bg: rgb(0x282a36),
    panel: rgb(0x21222c),
    text: rgb(0xf8f8f2),
    muted: rgb(0x8b91b0),
    border: rgb(0x6272a4),
    accent: rgb(0xbd93f9),
    green: rgb(0x50fa7b),
    red: rgb(0xff5555),
    add_bg: rgb(0x263d35),
    del_bg: rgb(0x482d36),
    hunk_bg: rgb(0x44475a),
    add_word_bg: rgb(0x285b40),
    del_word_bg: rgb(0x713c45),
    syntax: SyntaxColors {
        attribute: rgb(0xf1fa8c),
        number: rgb(0xbd93f9),
        comment: rgb(0x6272a4),
        type_color: rgb(0x8be9fd),
        keyword: rgb(0xff79c6),
        function: rgb(0x50fa7b),
        label: rgb(0xf1fa8c),
        operator: rgb(0xff79c6),
        punctuation: rgb(0xf8f8f2),
        string: rgb(0xf1fa8c),
        tag: rgb(0xff79c6),
    },
};

const NORD: Palette = Palette {
    bg: rgb(0x2e3440),
    panel: rgb(0x272c36),
    text: rgb(0xeceff4),
    muted: rgb(0x8b95a7),
    border: rgb(0x4c566a),
    accent: rgb(0x88c0d0),
    green: rgb(0xa3be8c),
    red: rgb(0xbf616a),
    add_bg: rgb(0x35433f),
    del_bg: rgb(0x46363f),
    hunk_bg: rgb(0x3b4252),
    add_word_bg: rgb(0x465e4b),
    del_word_bg: rgb(0x65414c),
    syntax: SyntaxColors {
        attribute: rgb(0xebcb8b),
        number: rgb(0xb48ead),
        comment: rgb(0x616e88),
        type_color: rgb(0x8fbcbb),
        keyword: rgb(0x81a1c1),
        function: rgb(0x88c0d0),
        label: rgb(0xebcb8b),
        operator: rgb(0x81a1c1),
        punctuation: rgb(0xd8dee9),
        string: rgb(0xa3be8c),
        tag: rgb(0xbf616a),
    },
};

const GRUVBOX: Palette = Palette {
    bg: rgb(0x282828),
    panel: rgb(0x1d2021),
    text: rgb(0xebdbb2),
    muted: rgb(0xa89984),
    border: rgb(0x665c54),
    accent: rgb(0x83a598),
    green: rgb(0xb8bb26),
    red: rgb(0xfb4934),
    add_bg: rgb(0x323a24),
    del_bg: rgb(0x462924),
    hunk_bg: rgb(0x3c3836),
    add_word_bg: rgb(0x4a5427),
    del_word_bg: rgb(0x733b2e),
    syntax: SyntaxColors {
        attribute: rgb(0xfabd2f),
        number: rgb(0xd3869b),
        comment: rgb(0x928374),
        type_color: rgb(0x8ec07c),
        keyword: rgb(0xfb4934),
        function: rgb(0xb8bb26),
        label: rgb(0xfabd2f),
        operator: rgb(0xfe8019),
        punctuation: rgb(0xa89984),
        string: rgb(0xb8bb26),
        tag: rgb(0xfb4934),
    },
};

const CATPPUCCIN_MOCHA: Palette = Palette {
    bg: rgb(0x1e1e2e),
    panel: rgb(0x181825),
    text: rgb(0xcdd6f4),
    muted: rgb(0x7f849c),
    border: rgb(0x45475a),
    accent: rgb(0x89b4fa),
    green: rgb(0xa6e3a1),
    red: rgb(0xf38ba8),
    add_bg: rgb(0x283b37),
    del_bg: rgb(0x402c3e),
    hunk_bg: rgb(0x313244),
    add_word_bg: rgb(0x3b5a45),
    del_word_bg: rgb(0x65425b),
    syntax: SyntaxColors {
        attribute: rgb(0xf9e2af),
        number: rgb(0xfab387),
        comment: rgb(0x6c7086),
        type_color: rgb(0x94e2d5),
        keyword: rgb(0xcba6f7),
        function: rgb(0x89b4fa),
        label: rgb(0xf9e2af),
        operator: rgb(0x89dceb),
        punctuation: rgb(0xbac2de),
        string: rgb(0xa6e3a1),
        tag: rgb(0xf38ba8),
    },
};

const ONE_DARK: Palette = Palette {
    bg: rgb(0x282c34),
    panel: rgb(0x21252b),
    text: rgb(0xabb2bf),
    muted: rgb(0x828997),
    border: rgb(0x4b5263),
    accent: rgb(0x61afef),
    green: rgb(0x98c379),
    red: rgb(0xe06c75),
    add_bg: rgb(0x2d3c34),
    del_bg: rgb(0x442f38),
    hunk_bg: rgb(0x3e4451),
    add_word_bg: rgb(0x405741),
    del_word_bg: rgb(0x63404a),
    syntax: SyntaxColors {
        attribute: rgb(0xe5c07b),
        number: rgb(0xd19a66),
        comment: rgb(0x5c6370),
        type_color: rgb(0xe5c07b),
        keyword: rgb(0xc678dd),
        function: rgb(0x61afef),
        label: rgb(0xe5c07b),
        operator: rgb(0x56b6c2),
        punctuation: rgb(0xabb2bf),
        string: rgb(0x98c379),
        tag: rgb(0xe06c75),
    },
};

const SOLARIZED_DARK: Palette = Palette {
    bg: rgb(0x002b36),
    panel: rgb(0x073642),
    text: rgb(0x839496),
    muted: rgb(0x657b83),
    border: rgb(0x586e75),
    accent: rgb(0x268bd2),
    green: rgb(0x859900),
    red: rgb(0xdc322f),
    add_bg: rgb(0x173d32),
    del_bg: rgb(0x382f35),
    hunk_bg: rgb(0x16414b),
    add_word_bg: rgb(0x304f29),
    del_word_bg: rgb(0x633a36),
    syntax: SyntaxColors {
        attribute: rgb(0xb58900),
        number: rgb(0xd33682),
        comment: rgb(0x586e75),
        type_color: rgb(0x2aa198),
        keyword: rgb(0x859900),
        function: rgb(0x268bd2),
        label: rgb(0xb58900),
        operator: rgb(0x6c71c4),
        punctuation: rgb(0x93a1a1),
        string: rgb(0x2aa198),
        tag: rgb(0xcb4b16),
    },
};

const SOLARIZED_LIGHT: Palette = Palette {
    bg: rgb(0xfdf6e3),
    panel: rgb(0xeee8d5),
    text: rgb(0x586e75),
    muted: rgb(0x839496),
    border: rgb(0x93a1a1),
    accent: rgb(0x268bd2),
    green: rgb(0x527600),
    red: rgb(0xc62d2a),
    add_bg: rgb(0xe5edcf),
    del_bg: rgb(0xf8ded1),
    hunk_bg: rgb(0xe4decb),
    add_word_bg: rgb(0xc9daa7),
    del_word_bg: rgb(0xedb9a9),
    syntax: SyntaxColors {
        attribute: rgb(0x946e00),
        number: rgb(0xb6296f),
        comment: rgb(0x839496),
        type_color: rgb(0x217f78),
        keyword: rgb(0x6c7100),
        function: rgb(0x2075b1),
        label: rgb(0x946e00),
        operator: rgb(0x5b60ab),
        punctuation: rgb(0x657b83),
        string: rgb(0x217f78),
        tag: rgb(0xb54313),
    },
};

const GITHUB_LIGHT: Palette = Palette {
    bg: rgb(0xffffff),
    panel: rgb(0xf6f8fa),
    text: rgb(0x1f2328),
    muted: rgb(0x656d76),
    border: rgb(0xd0d7de),
    accent: rgb(0x0969da),
    green: rgb(0x1a7f37),
    red: rgb(0xcf222e),
    add_bg: rgb(0xdafbe1),
    del_bg: rgb(0xffebe9),
    hunk_bg: rgb(0xeaeef2),
    add_word_bg: rgb(0xabf2bc),
    del_word_bg: rgb(0xffc1ba),
    syntax: SyntaxColors {
        attribute: rgb(0x953800),
        number: rgb(0x0550ae),
        comment: rgb(0x6e7781),
        type_color: rgb(0x953800),
        keyword: rgb(0xcf222e),
        function: rgb(0x8250df),
        label: rgb(0x953800),
        operator: rgb(0xcf222e),
        punctuation: rgb(0x57606a),
        string: rgb(0x0a3069),
        tag: rgb(0x116329),
    },
};
