pub const CPF_INPUT: u32 = 0x4;
pub const CPF_OPTIONAL_PARM: u32 = 0x10;
pub const CPF_NET: u32 = 0x20;
pub const CPF_PARM: u32 = 0x80;
pub const CPF_OUT_PARM: u32 = 0x100;
pub const CPF_RETURN_PARM: u32 = 0x400;
pub const CPF_NATIVE: u32 = 0x1000;
pub const CPF_TRANSIENT: u32 = 0x2000;
pub const CPF_CONFIG: u32 = 0x4000;
pub const CPF_TRAVEL: u32 = 0x0001_0000;
/// Loaded and saved in the section of the declaring class, not the object's class (unverified).
pub const CPF_GLOBAL_CONFIG: u32 = 0x0004_0000;
pub const CPF_COMMENT_STRING: u32 = 0x0200_0000;

pub const FUNC_LATENT: u32 = 0x08;
pub const FUNC_PRE_OPERATOR: u32 = 0x10;
pub const FUNC_SINGULAR: u32 = 0x20;
pub const FUNC_NET: u32 = 0x40;
pub const FUNC_EXEC: u32 = 0x200;
pub const FUNC_NATIVE: u32 = 0x400;
pub const FUNC_OPERATOR: u32 = 0x1000;

pub const STATE_AUTO: u32 = 0x02;

pub const CLASS_CONFIG: u32 = 0x4;
