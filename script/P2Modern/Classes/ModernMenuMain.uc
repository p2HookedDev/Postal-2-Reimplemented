// The host swaps this class in for `Shell.MenuMain`. Retail shows the POSTAL 4 or Paradise Lost banner only in
// a Steam build, which would also send achievements to Steam; ours shows it from what the host says Steam owns.
class ModernMenuMain extends MenuMain;

// Super resets the title only when its class is exactly MenuMain.
function CreateMenuContents()
{
	TitleTexture = Default.TitleTexture;
	Super.CreateMenuContents();
}

function AddSocialIcons()
{
	Super.AddSocialIcons();

	if (!PlatformIsWindows() || Root.WinWidth == 0 || Root.WinHeight == 0)
		return;

	if (!GetLevel().SteamOwnsGame(P4_GAME_APPID))
		Postal4Icon = AddBanner(Postal4Texture);
	else if (!GetLevel().SteamOwnsDLC(PL_DLC_APPID))
		ParadiseLostIcon = AddBanner(ParadiseLostTexture);
}

function ShellBitmapSocial AddBanner(Texture BannerTexture)
{
	local ShellBitmapSocial Banner;

	Banner = ShellBitmapSocial(Root.CreateWindow(class'ShellBitmapSocial', Root.WinWidth * 0.02, Root.WinHeight * 0.92 - 255, 300, 235));
	Banner.bStretch = true;
	Banner.bCenter = true;
	Banner.T = BannerTexture;
	Banner.R.W = BannerTexture.USize;
	Banner.R.H = BannerTexture.VSize;
	Banner.MyMenu = Self;
	return Banner;
}
