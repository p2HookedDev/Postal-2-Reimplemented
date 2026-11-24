// No preset writes these rows, and changing one keeps the preset.
class ModernMenuScreen extends ModernMenuPage;

var localized string    HudNames[3];

function CreateMenuContents()
{
	local UWindowComboControl Combo;
	local int i;

	Super.CreateMenuContents();

	AddTitle(TitleText, TitleFont, TitleAlign);

	bUpdate = false;
	Combo = AddLevelCombo(0, "ModernSharpHud", 1, 2);
	for (i = 0; i < ArrayCount(HudNames); i++)
		Combo.AddItem(HudNames[i]);
	AddBox(1, "ModernWideView");
	AddBox(2, "ModernHudSafeArea");
	AddLevelSlider(3, "ModernPanini", 0, 10, 0);
	AddLevelSlider(4, "ModernFadeIn", 0, 10, 0);
	AddBack();
	LoadRows();
}

function RowChanged(int i)
{
}

defaultproperties
{
	TitleText="Screen and HUD"
	BackHelp="Return to the Modern Graphics menu."

	HudNames(0)="Original"
	HudNames(1)="Pixel perfect"
	HudNames(2)="Smooth text"

	Labels(0)="HUD scaling"
	Labels(1)="Widescreen FOV"
	Labels(2)="Centred HUD"
	Labels(3)="Panini projection"
	Labels(4)="Draw distance fade"

	Helps(0)="Pixel perfect keeps the HUD crisp; Smooth text also smooths its lettering."
	Helps(1)="Wide screens show more at the sides instead of cropping top and bottom."
	Helps(2)="Keeps the HUD in a 16:9 area in the middle of wide screens."
	Helps(3)="Reduces stretching at the screen edges with a wide field of view."
	Helps(4)="Objects fade out near the draw distance instead of popping; higher widens the fade."
}
