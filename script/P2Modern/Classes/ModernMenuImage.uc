class ModernMenuImage extends ModernMenuPage;

var localized string    SmoothingNames[3];

function CreateMenuContents()
{
	local UWindowComboControl Combo;
	local int i;

	Super.CreateMenuContents();

	AddTitle(TitleText, TitleFont, TitleAlign);

	bUpdate = false;
	Combo = AddLevelCombo(0, "ModernAntiAliasing", 2, 2);
	for (i = 0; i < ArrayCount(SmoothingNames); i++)
		Combo.AddItem(SmoothingNames[i]);
	AddLevelSlider(1, "ModernBloom", 0, 10, 5);
	AddLevelSlider(2, "ModernSharpen", 0, 10, 4);
	AddBack();
	LoadRows();
}

defaultproperties
{
	TitleText="Image Quality"
	BackHelp="Return to the Modern Graphics menu."

	SmoothingNames(0)="Off"
	SmoothingNames(1)="FXAA"
	SmoothingNames(2)="SMAA"

	Labels(0)="Antialiasing"
	Labels(1)="Bloom"
	Labels(2)="Sharpening"

	Helps(0)="Smooths jagged edges; FXAA is fastest, SMAA sharper."
	Helps(1)="A glow around bright lights."
	Helps(2)="Makes the picture crisper."
}
